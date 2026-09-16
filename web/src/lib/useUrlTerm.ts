// SPDX-License-Identifier: AGPL-3.0-only
// A free-text search term that lives in the URL, behind a box the operator types into (ADR-153).
//
// **Why this is not `useFilterParams`.** A filter row's cell already owns a draft and commits on
// the settle (`TextConditionEditor`), so the URL hook underneath it never sees a keystroke. A bare
// search box has no such cell: the box's `value` is the only state there is. Binding that `value`
// straight to the URL would write the query string once per keystroke, which is three things wrong
// at once:
//
//  1. **Safari throws.** `history.replaceState` past 100 calls in 30 seconds is a `SecurityError`,
//     and this app has no error boundary — a long retype is a blank page.
//  2. **The URL is trimmed; the box must not be.** "sw " is a legitimate thing to be halfway through
//     typing. A box fed back its own trimmed value eats the space under the caret, and an IME
//     composition fed back a lagging value loses the characters being composed.
//  3. **Every URL change is a write elsewhere.** `AppShell` records the section's current route on
//     every change of `location.search`.
//
// So the box holds a **draft**, and the URL receives the term only once it has settled — the
// caller says when, because the caller already has a settle of its own (the tree's
// `useFilterSearch`, the MIB repository's `useDebouncedValue`). A second timer here would be a
// second answer to "when has the operator stopped typing", and the two could disagree by a tick.
//
// In a `.ts` because the rule for adopting a URL change is exactly the kind of thing that reads fine
// and is wrong; `useUrlTerm.test.ts` runs it.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { readIdParam, writeIdParam } from './filterParams';

export interface UrlTerm {
  /** What the box shows. Local, never trimmed. */
  draft: string;
  setDraft: (next: string) => void;
  /**
   * Write a settled term to the URL — trimmed, and deleting the key when it is blank.
   *
   * Stable identity, so a caller can commit from an effect keyed on its settled value alone.
   * ⚠️ Do not key that effect on anything that changes with the URL: a Back navigation lands a new
   * term in the URL while the caller's settled value is still the old one, and an effect that re-ran
   * on the URL change would write the old term straight back over the navigation.
   */
  commit: (settled: string) => void;
  /**
   * Put `term` into a `URLSearchParams` the caller is about to commit itself, and into the box.
   *
   * For a handler that writes more than this key — "clear all filters" clears the term and three
   * other keys, and that has to be **one** `setSearchParams` (ADR-053: two writes from one render's
   * snapshot and the second restores what the first cleared). Recording the term here is what stops
   * the caller's next `commit` from issuing a write of its own built from the pre-clear snapshot.
   */
  assign: (params: URLSearchParams, term: string) => void;
}

export function useUrlTerm(key: string): UrlTerm {
  const [params, setParams] = useSearchParams();
  const urlValue = readIdParam(params, key) ?? '';
  const [draft, setDraft] = useState(urlValue);
  /** The value this hook last wrote (or adopted). Its own write arriving is not a change to adopt. */
  const echo = useRef(urlValue);
  const latest = useRef({ params, setParams, key });
  latest.current = { params, setParams, key };

  // Adopt a term that arrived from outside — Back/Forward, a link, "clear all filters". Two
  // conditions, and each one is load-bearing:
  //  - **the dependency array is `[urlValue]` and nothing else**, so this runs only when the URL's
  //    term changed. It is NOT "the URL differs from the box": a write that was *lost* (another
  //    `setSearchParams` in the same tick, built from an older snapshot, landing last) leaves the URL
  //    on the term it already had, and adopting that would snap the box back under the operator's
  //    fingers. Here the term did not change, so nothing runs and the box keeps what was typed.
  //  - **our own write is not a change to adopt** — it is the trimmed term, and adopting it would eat
  //    the trailing space of "sw " the moment it settled.
  useEffect(() => {
    if (urlValue === echo.current) return;
    echo.current = urlValue;
    setDraft(urlValue);
  }, [urlValue]);

  const commit = useCallback((settled: string) => {
    const term = settled.trim();
    if (term === echo.current) return;
    echo.current = term;
    const { params: current, setParams: write, key: k } = latest.current;
    const next = new URLSearchParams(current);
    writeIdParam(next, k, term || null);
    // `replace`: a settled term is not a place to go Back to (`filterParams.ts`).
    write(next, { replace: true });
  }, []);

  const assign = useCallback((target: URLSearchParams, term: string) => {
    const trimmed = term.trim();
    echo.current = trimmed;
    writeIdParam(target, latest.current.key, trimmed || null);
    setDraft(term);
  }, []);

  return { draft, setDraft, commit, assign };
}
