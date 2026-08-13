// SPDX-License-Identifier: AGPL-3.0-only
// The free-text half of a column filter: a term, a match mode, and a negation toggle (ADR-053).
//
// All three of those are one URL key, and the codec that makes that work is `lib/filterCondition.ts`
// — every judgement lives there, where tests actually run. This file is the controls.
//
// Two behaviours worth knowing before editing:
//   - **The term is debounced; the mode and the NOT toggle are not.** A keystroke is a draft, so
//     committing each one would fire a request per character on a server-side list and push a
//     history entry per character on every list. A click on Regex or Exclude is a decision, and
//     waiting 250ms after a deliberate click reads as lag.
//   - **An invalid regex is shown, not thrown.** `[` is a state every regex passes through while it
//     is being typed; `compileCondition` stays total and matches nothing, and the message goes here
//     beside the box.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Segmented } from './Segmented';
import type { TextMode } from '../../lib/columnFilter';
import { conditionError, type TextCondition } from '../../lib/filterCondition';
import './TextConditionEditor.css';

interface Props {
  value: TextCondition;
  onChange: (next: TextCondition) => void;
  /** Modes this column offers. A single-mode column renders no switch. */
  modes: readonly TextMode[];
  /** Whether excluding is meaningful for this column. */
  allowNot?: boolean;
  placeholder?: string;
  /** What a plain term means on this list. `'token'` earns a hint: on a VictoriaLogs deployment
   *  `POLICY` does not find `POLICYPERMIT`, and nothing else on screen would explain that. */
  containsSemantics?: 'substring' | 'token';
  autoFocus?: boolean;
}

const DEBOUNCE_MS = 250;

export function TextConditionEditor({
  value,
  onChange,
  modes,
  allowNot = false,
  placeholder,
  containsSemantics,
  autoFocus,
}: Props) {
  const { t } = useTranslation('common');
  const [draft, setDraft] = useState(value.term);
  /** The term this component last committed. Without it, the sync-from-props effect below would
   *  fight the debounce: the commit re-enters as a new `value.term` and would reset the draft the
   *  operator has kept typing into. */
  const committed = useRef(value.term);
  /** ⚠️ `value` and `onChange` are read through a ref, and the debounce effect depends on `draft`
   *  ALONE. Both props change identity on nearly every parent render — `value` is derived from the
   *  URL, `onChange` is an inline closure — so listing them as dependencies restarts the timer each
   *  render and the term is never committed at all. The bug is invisible in a fast test and total
   *  in a live app. */
  const latest = useRef({ value, onChange });
  latest.current = { value, onChange };

  // Adopt an externally-changed term (the cell's clear button, a back/forward navigation), but not
  // the echo of our own commit.
  useEffect(() => {
    if (value.term !== committed.current) {
      committed.current = value.term;
      setDraft(value.term);
    }
  }, [value.term]);

  useEffect(() => {
    if (draft === committed.current) return;
    const id = setTimeout(() => {
      committed.current = draft;
      latest.current.onChange({ ...latest.current.value, term: draft });
    }, DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [draft]);

  const commitNow = (next: TextCondition) => {
    committed.current = next.term;
    onChange(next);
  };

  const error = conditionError({ ...value, term: draft });
  const showHint = containsSemantics === 'token' && value.mode === 'contains';

  return (
    <div className="tcond">
      <input
        type="search"
        className="tcond-input"
        value={draft}
        placeholder={placeholder ?? t('filter.termPlaceholder')}
        aria-label={placeholder ?? t('filter.termPlaceholder')}
        autoFocus={autoFocus}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          // Enter commits immediately rather than waiting out the debounce — the operator has said
          // they are done.
          if (e.key === 'Enter') commitNow({ ...value, term: draft });
        }}
      />
      {(modes.length > 1 || allowNot) && (
        <div className="tcond-controls">
          {modes.length > 1 && (
            <Segmented
              options={modes.map((m) => ({ value: m, label: t(`filter.mode.${m}`) }))}
              value={value.mode}
              onChange={(m) => commitNow({ ...value, term: draft, mode: m as TextMode })}
              ariaLabel={t('filter.modeAria')}
            />
          )}
          {allowNot && (
            <button
              type="button"
              className={value.not ? 'tcond-not on' : 'tcond-not'}
              aria-pressed={value.not}
              title={t('filter.notAria')}
              onClick={() => commitNow({ ...value, term: draft, not: !value.not })}
            >
              {t('filter.not')}
            </button>
          )}
        </div>
      )}
      {error && <p className="tcond-error">{t('filter.regexInvalid', { message: error })}</p>}
      {!error && showHint && <p className="tcond-hint">{t('filter.tokenHint')}</p>}
    </div>
  );
}
