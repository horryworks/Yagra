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
//   - **The mode is a switch, not a two-option segmented control.** `TEXT_MODES` has exactly two
//     members and always will — "contains" is the absence of "regex", not a peer choice — and a
//     segmented control spells a boolean as if it were a set, which is why the shipped version left
//     an operator unable to tell which half was active (the selected fill is one step of background
//     tone apart). A switch states the *default* too: regex is off unless you turn it on.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TextMode } from '../../lib/columnFilter';
import {
  conditionEcho,
  conditionError,
  conditionIsActive,
  type TextCondition,
} from '../../lib/filterCondition';
import './TextConditionEditor.css';

interface Props {
  value: TextCondition;
  onChange: (next: TextCondition) => void;
  /** Modes this column offers. A single-mode column renders no switch. */
  modes: readonly TextMode[];
  /** Whether excluding is meaningful for this column. */
  allowNot?: boolean;
  placeholder?: string;
  /** What a plain term means on this list. `'prefix'` earns a hint: on a VictoriaLogs deployment a
   *  term matches from the start of a word, so `PERMIT` does not find `POLICYPERMIT` (`POLICY`
   *  does), and nothing else on screen would explain that. */
  containsSemantics?: 'substring' | 'prefix';
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
  /** **The whole condition is draft state, not just the term.** Mode and negation are meaningful
   *  before there is anything to match — an operator turns on Regex and *then* types a pattern —
   *  but they cannot be stored: an inactive condition encodes to `''` and the key is deleted, so
   *  the only place they can live until a term exists is here. Keeping only the term here is what
   *  made the switch appear frozen on an empty box. */
  const [draft, setDraft] = useState<TextCondition>(value);
  /** What the parent will report back after our last commit — `conditionEcho`, not what we sent.
   *  Without it the sync-from-props effect below fights the debounce (the commit re-enters as a new
   *  `value` and resets the draft the operator is still typing into) and, for an inactive
   *  condition, undoes the mode they just chose. */
  const echo = useRef<TextCondition>(conditionEcho(value));
  /** ⚠️ `value` and `onChange` are read through a ref, and the debounce effect depends on `draft`
   *  ALONE. Both props change identity on nearly every parent render — `value` is derived from the
   *  URL, `onChange` is an inline closure — so listing them as dependencies restarts the timer each
   *  render and the term is never committed at all. The bug is invisible in a fast test and total
   *  in a live app. */
  const latest = useRef({ value, onChange });
  latest.current = { value, onChange };
  /** The debounce fires after the render that scheduled it, so it must read the *current* draft
   *  rather than the one captured when the timer was set. */
  const draftRef = useRef(draft);
  draftRef.current = draft;

  // Adopt an externally-changed condition (the cell's clear button, a back/forward navigation), but
  // not the echo of our own commit.
  useEffect(() => {
    // Read through the ref rather than the prop: the three primitives are the real dependencies,
    // and naming `value` itself would re-run this on every parent render (new object identity).
    const next = latest.current.value;
    const e = echo.current;
    if (next.term !== e.term || next.mode !== e.mode || next.not !== e.not) {
      echo.current = next;
      setDraft(next);
    }
  }, [value.term, value.mode, value.not]);

  const commit = (next: TextCondition) => {
    const wasActive = conditionIsActive(echo.current);
    echo.current = conditionEcho(next);
    // Flipping a switch on an empty box changes nothing the URL can hold, so do not write one —
    // that would push a history entry per click for a filter that is not filtering.
    if (!wasActive && !conditionIsActive(next)) return;
    latest.current.onChange(next);
  };

  useEffect(() => {
    if (draft.term === echo.current.term) return;
    const id = setTimeout(() => commit(draftRef.current), DEBOUNCE_MS);
    return () => clearTimeout(id);
  }, [draft.term]);

  const commitNow = (next: TextCondition) => {
    setDraft(next);
    commit(next);
  };

  const error = conditionError(draft);
  const showHint = containsSemantics === 'prefix' && draft.mode === 'contains';

  return (
    <div className="tcond">
      <input
        type="search"
        className="tcond-input"
        value={draft.term}
        placeholder={placeholder ?? t('filter.termPlaceholder')}
        aria-label={placeholder ?? t('filter.termPlaceholder')}
        autoFocus={autoFocus}
        onChange={(e) => setDraft({ ...draft, term: e.target.value })}
        onKeyDown={(e) => {
          // Enter commits immediately rather than waiting out the debounce — the operator has said
          // they are done.
          if (e.key === 'Enter') commitNow(draft);
        }}
      />
      {(modes.length > 1 || allowNot) && (
        <div className="tcond-controls">
          {modes.length > 1 && (
            <label
              className="tcond-sw"
              title={`${t('filter.modeAria')}: ${t(`filter.mode.${draft.mode}`)}`}
            >
              <input
                type="checkbox"
                role="switch"
                checked={draft.mode === 'regex'}
                onChange={(e) =>
                  commitNow({
                    ...draft,
                    mode: (e.target.checked ? 'regex' : 'contains') satisfies TextMode,
                  })
                }
              />
              <span className="tcond-sw-track" aria-hidden="true" />
              {t('filter.mode.regex')}
            </label>
          )}
          {allowNot && (
            <button
              type="button"
              className={draft.not ? 'tcond-not on' : 'tcond-not'}
              aria-pressed={draft.not}
              title={t('filter.notAria')}
              onClick={() => commitNow({ ...draft, not: !draft.not })}
            >
              {t('filter.not')}
            </button>
          )}
        </div>
      )}
      {error && <p className="tcond-error">{t('filter.regexInvalid', { message: error })}</p>}
      {!error && showHint && <p className="tcond-hint">{t('filter.prefixHint')}</p>}
    </div>
  );
}
