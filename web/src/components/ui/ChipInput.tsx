// SPDX-License-Identifier: AGPL-3.0-only
import { useCallback, useId, useRef, useState } from 'react';
import type { KeyboardEvent, ClipboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { TextInput, FieldError } from './Field';
import { IconButton } from './IconButton';
import { AnchoredPopover } from './AnchoredPopover';
import { canonicalLabel, enterValue, moveActive, shownSuggestions } from './chipSuggest';
import {
  LABELS_MAX,
  addLabel,
  labelProblem,
  labelProblems,
  removeLabel,
  splitPastedLabels,
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
   * Values the entry box offers (ADR-263 Inc.2: the registered tags). Offered, never required: a
   * value outside the list is accepted as before. One already in the list is not offered again.
   * Since Inc.3 they open as a list under the box as soon as it has focus, narrowed by what is
   * typed; a pick becomes a chip at once.
   */
  suggestions?: readonly string[];
}

export function ChipInput({
  value,
  onChange,
  placeholder,
  inputLabel,
  lenient = false,
  disabled = false,
  suggestions = [],
}: Props) {
  const { t } = useTranslation('nodes');
  const [draft, setDraft] = useState('');
  // The list is open while the box has focus, unless Escape closed it; typing or clicking the box
  // opens it again. A native `datalist` was used before Inc.3 and showed nothing until a match was
  // typed, so an operator who clicked the box saw no suggestions at all.
  const [focused, setFocused] = useState(false);
  const [dismissed, setDismissed] = useState(false);
  const [active, setActive] = useState(-1);
  const anchorRef = useRef<HTMLDivElement>(null);
  const listId = useId();
  const shown = shownSuggestions(suggestions, value, draft);
  const open = focused && !dismissed && !disabled && shown.length > 0;
  const onDismiss = useCallback(() => setDismissed(true), []);

  // Marked per chip rather than blocking the whole dialog: migration 0109 can hand back a label
  // longer than the rules now allow, and an operator who cannot save *and* cannot see why has no
  // way forward. Marking it makes deleting that one chip the obvious fix.
  const problems = lenient ? new Map() : labelProblems(value);
  const draftProblem = draft.trim() === '' || lenient ? null : labelProblem(draft, value);

  const commit = (raw: string) => {
    // Every path folds a case-only variant onto the spelling already known — the chosen chips
    // first, then the suggestions — not only Enter on an open list.
    const known = [...value, ...suggestions];
    const labels = splitPastedLabels(raw).map((l) => canonicalLabel(l, known));
    const next = lenient
      ? [...value, ...labels.filter((l) => !value.includes(l))]
      : labels.reduce<string[]>((acc, l) => addLabel(acc, l), [...value]);
    if (next.length !== value.length) onChange(next);
    setDraft('');
    setActive(-1);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    // An IME owns the key while it is composing — Enter confirms a conversion, the arrows walk its
    // candidates. See `lib/ime.ts`.
    if (isImeComposing(e)) return;
    if (open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      setActive((a) => moveActive(a, shown.length, e.key === 'ArrowDown' ? 1 : -1));
      return;
    }
    if (e.key === 'ArrowDown' && dismissed) {
      e.preventDefault();
      setDismissed(false);
      return;
    }
    if (e.key === 'Enter' || e.key === ',') {
      // Enter would submit the dialog and `,` would land in the box; both mean "that is one label".
      e.preventDefault();
      const picked = enterValue(open ? shown : [], open ? active : -1, draft);
      if (picked !== null) commit(picked);
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
                className={problem ? 'chipinput-chip chipinput-chip-bad' : 'chipinput-chip'}
                title={problem ? t(`field.tagErr.${problem}`) : undefined}
              >
                <span>{label}</span>
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
      <div ref={anchorRef} className="chipinput-entry">
        <TextInput
          value={draft}
          placeholder={placeholder}
          aria-label={inputLabel}
          role={suggestions.length > 0 ? 'combobox' : undefined}
          aria-haspopup={suggestions.length > 0 ? 'listbox' : undefined}
          aria-expanded={suggestions.length > 0 ? open : undefined}
          aria-controls={open ? listId : undefined}
          aria-activedescendant={open && active >= 0 ? `${listId}-${active}` : undefined}
          autoComplete="off"
          disabled={disabled || (!lenient && value.length >= LABELS_MAX)}
          onChange={(e) => {
            setDraft(e.target.value);
            setActive(-1);
            setDismissed(false);
          }}
          onFocus={() => setFocused(true)}
          onClick={() => setDismissed(false)}
          onKeyDown={onKeyDown}
          onPaste={onPaste}
          // Committing on blur as well as on Enter: a half-typed label the operator then clicks Save
          // on would otherwise be dropped without a word.
          onBlur={() => {
            setFocused(false);
            setDismissed(false);
            if (draft.trim() !== '' && draftProblem === null) commit(draft);
          }}
        />
      </div>
      <AnchoredPopover
        open={open}
        anchorRef={anchorRef}
        role="listbox"
        label={t('field.tagSuggestions')}
        onDismiss={onDismiss}
        align="start"
        className="chipinput-pop"
        id={listId}
      >
        <div>
          {shown.map((s, i) => (
            <div
              key={s}
              id={`${listId}-${i}`}
              role="option"
              aria-selected={i === active}
              className={i === active ? 'chipinput-option active' : 'chipinput-option'}
              // Keep focus in the box: a blur would commit the half-typed text first.
              onMouseDown={(e) => e.preventDefault()}
              onMouseEnter={() => setActive(i)}
              onClick={() => commit(s)}
            >
              {s}
            </div>
          ))}
        </div>
      </AnchoredPopover>
      {draftProblem && <FieldError>{t(`field.tagErr.${draftProblem}`)}</FieldError>}
    </div>
  );
}
