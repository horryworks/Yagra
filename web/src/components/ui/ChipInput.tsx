// SPDX-License-Identifier: AGPL-3.0-only
import { useState } from 'react';
import type { KeyboardEvent, ClipboardEvent } from 'react';
import { useTranslation } from 'react-i18next';
import { TextInput, FieldHint } from './Field';
import { IconButton } from './IconButton';
import {
  LABELS_MAX,
  addLabel,
  labelProblem,
  labelProblems,
  removeLabel,
  splitPastedLabels,
} from './labelRules';
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
}

export function ChipInput({
  value,
  onChange,
  placeholder,
  inputLabel,
  lenient = false,
  disabled = false,
}: Props) {
  const { t } = useTranslation('nodes');
  const [draft, setDraft] = useState('');

  // Marked per chip rather than blocking the whole dialog: migration 0109 can hand back a label
  // longer than the rules now allow, and an operator who cannot save *and* cannot see why has no
  // way forward. Marking it makes deleting that one chip the obvious fix.
  const problems = lenient ? new Map() : labelProblems(value);
  const draftProblem = draft.trim() === '' || lenient ? null : labelProblem(draft, value);

  const commit = (raw: string) => {
    const next = lenient
      ? [...value, ...splitPastedLabels(raw).filter((l) => !value.includes(l))]
      : splitPastedLabels(raw).reduce<string[]>((acc, l) => addLabel(acc, l), [...value]);
    if (next.length !== value.length) onChange(next);
    setDraft('');
  };

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter' || e.key === ',') {
      // Enter would submit the dialog and `,` would land in the box; both mean "that is one label".
      e.preventDefault();
      if (draft.trim() !== '') commit(draft);
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
      <TextInput
        value={draft}
        placeholder={placeholder}
        aria-label={inputLabel}
        disabled={disabled || (!lenient && value.length >= LABELS_MAX)}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
        // Committing on blur as well as on Enter: a half-typed label the operator then clicks Save
        // on would otherwise be dropped without a word.
        onBlur={() => {
          if (draft.trim() !== '' && draftProblem === null) commit(draft);
        }}
      />
      {draftProblem && <FieldHint error>{t(`field.tagErr.${draftProblem}`)}</FieldHint>}
    </div>
  );
}
