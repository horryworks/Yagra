// SPDX-License-Identifier: AGPL-3.0-only
import { useRef, useState } from 'react';
import type { KeyboardEvent, ClipboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { TextInput, FieldError } from './Field';
import { IconButton } from './IconButton';
import { AnchoredPopover } from './AnchoredPopover';
import {
  LABELS_MAX,
  addLabel,
  labelProblem,
  labelProblems,
  matchingOptions,
  removeLabel,
  resolveOption,
  splitPastedLabels,
  valuesWithoutOption,
  type ChipOption,
} from './labelRules';
import { isImeComposing } from '../../lib/ime';
import './ChipInput.css';

// A list of short free-text values edited as removable chips (ADR-135 inc. 2).
//
// 🚨 **Shared rather than local, because this is the fourth time the shape was needed and the
// third time it was written.** `.thresholds-chip` (the threshold target picker) and `.nd-tag-chip`
// (the node tag editor) were two independent implementations of one control, and the tag editor
// itself was then copied verbatim into the bulk dialog. Those rules moved here; the callers render
// this.
//
// All judgement lives in `./labelRules.ts`, which Vitest can reach — this file is layout and
// keyboard handling only.
//
// ⚠️ **That file is not called `chipInput.ts`, and the name is forced.** On Windows a `.ts` and a
// `.tsx` differing only in case collide (TS1149) and the whole program stops compiling — the trap
// `globalSearch.ts` / `GlobalSearch.tsx` already paid for once.

interface Props {
  /** The current list. Sorted or not as the caller likes; this never reorders it. */
  value: readonly string[];
  onChange: (next: string[]) => void;
  /** Placeholder for the entry box. */
  placeholder?: string;
  /** Accessible name for the entry box — required, since the visible label belongs to the field. */
  inputLabel: string;
  /**
   * Skip the length/character rules on entry.
   *
   * For a list of values to **remove**: one already stored may predate the rules that now apply to
   * new labels, so refusing to let it be typed would make exactly the values somebody wants gone
   * impossible to name. `api/nodes.rs::normalized_removals` is the same decision server-side.
   */
  lenient?: boolean;
  disabled?: boolean;
  /**
   * Pick from these rather than type freely (ADR-259 Inc.2). Only a value one of them names can be
   * added — spelled as the option is — and a chosen value none offers is marked, so a typo cannot
   * be saved and a tag that has since gone from every node is visible. Absent = free entry.
   */
  options?: readonly ChipOption[];
}

export function ChipInput({
  value,
  onChange,
  placeholder,
  inputLabel,
  lenient = false,
  disabled = false,
  options,
}: Props) {
  const { t } = useTranslation('nodes');
  const [draft, setDraft] = useState('');
  const [focused, setFocused] = useState(false);
  const anchorRef = useRef<HTMLDivElement>(null);
  // A typed value no option names, kept to say so under the box (options mode only).
  const [unknown, setUnknown] = useState<string | null>(null);
  const offered = options ? matchingOptions(options, value, draft) : [];
  const gone = options ? valuesWithoutOption(value, options) : new Set<string>();

  // Marked per chip rather than blocking the whole dialog: migration 0109 can hand back a label
  // longer than the rules now allow, and an operator who cannot save *and* cannot see why has no
  // way forward. Marking it makes deleting that one chip the obvious fix.
  const problems = lenient ? new Map() : labelProblems(value);
  const draftProblem = draft.trim() === '' || lenient ? null : labelProblem(draft, value);

  const commit = (raw: string) => {
    if (options) {
      // Each pasted or typed value becomes the option it names; one naming none stays in the box,
      // said under it, and is not added.
      let next = [...value];
      let missed: string | null = null;
      for (const l of splitPastedLabels(raw)) {
        const hit = resolveOption(options, l);
        if (hit === null) missed ??= l;
        else next = addLabel(next, hit);
      }
      if (next.length !== value.length) onChange(next);
      setUnknown(missed);
      setDraft(missed ?? '');
      return;
    }
    const next = lenient
      ? [...value, ...splitPastedLabels(raw).filter((l) => !value.includes(l))]
      : splitPastedLabels(raw).reduce<string[]>((acc, l) => addLabel(acc, l), [...value]);
    if (next.length !== value.length) onChange(next);
    setDraft('');
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // An IME owns the key while it is composing — Enter confirms a conversion, the arrows walk its
    // candidates. See `lib/ime.ts`.
    if (isImeComposing(e)) return;
    if (e.key === 'Enter' || e.key === ',') {
      // Enter would submit the dialog and `,` would land in the box; both mean "that is one label".
      e.preventDefault();
      if (draft.trim() === '') return;
      // In options mode Enter takes the value typed if it names one, else the first offered.
      const pick = options ? (resolveOption(options, draft) ?? offered[0]?.value) : undefined;
      commit(pick ?? draft);
      return;
    }
    if (e.key === 'Backspace' && draft === '' && value.length > 0) {
      // The usual chip-field gesture: backspace on an empty box takes the last one back.
      onChange(value.slice(0, -1));
    }
  };

  const onPaste = (e: ClipboardEvent<HTMLInputElement>) => {
    const text = e.clipboardData.getData('text');
    if (!/[,\n\r]/.test(text)) return; // one value: let the box handle it normally
    e.preventDefault();
    commit(text);
  };

  return (
    <div className="chipinput">
      {value.length > 0 && (
        <ul className="chipinput-chips">
          {value.map((label) => {
            const problem = problems.get(label);
            return (
              <li
                key={label}
                className={
                  problem || gone.has(label) ? 'chipinput-chip chipinput-chip-bad' : 'chipinput-chip'
                }
                title={problem ? t(`field.tagErr.${problem}`) : undefined}
              >
                <span>{label}</span>
                {gone.has(label) && <span>· {t('field.tagNoNode')}</span>}
                <IconButton
                  title={t('field.tagRemove', { label })}
                  disabled={disabled}
                  onClick={() => onChange(removeLabel(value, label))}
                >
                  ✕
                </IconButton>
              </li>
            );
          })}
        </ul>
      )}
      <div ref={anchorRef}>
        <TextInput
          value={draft}
          placeholder={placeholder}
          aria-label={inputLabel}
          // The popover finds the box to place itself under by this mark, and a screen reader
          // learns the box opens a list of choices.
          aria-haspopup={options ? 'listbox' : undefined}
          aria-expanded={options ? focused && offered.length > 0 : undefined}
          disabled={disabled || (!lenient && value.length >= LABELS_MAX)}
          onChange={(e) => {
            setDraft(e.target.value);
            setUnknown(null);
            setFocused(true);
          }}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          onFocus={() => setFocused(true)}
          // Committing on blur as well as on Enter: a half-typed label the operator then clicks Save
          // on would otherwise be dropped without a word.
          onBlur={() => {
            setFocused(false);
            if (draft.trim() !== '' && draftProblem === null) commit(draft);
          }}
        />
      </div>
      {/* Floating, not in the flow: a list that pushed the fields down would vanish on blur and
          pull the dialog's Save button up from under the pointer mid-click. Each choice takes the
          press on mousedown so the box keeps its focus. */}
      {options && (
        <AnchoredPopover
          open={focused && offered.length > 0}
          anchorRef={anchorRef}
          role="listbox"
          label={inputLabel}
          align="start"
          className="chipinput-options"
          onDismiss={() => setFocused(false)}
        >
          {offered.map((o) => (
            <button
              key={o.value}
              type="button"
              role="option"
              aria-selected={false}
              className="chipinput-option"
              onMouseDown={(e) => {
                e.preventDefault();
                commit(o.value);
              }}
            >
              <span>{o.value}</span>
              {o.detail && <span className="muted">{o.detail}</span>}
            </button>
          ))}
        </AnchoredPopover>
      )}
      {unknown !== null && <FieldError>{t('field.tagNotCarried', { tag: unknown })}</FieldError>}
      {draftProblem && <FieldError>{t(`field.tagErr.${draftProblem}`)}</FieldError>}
    </div>
  );
}
