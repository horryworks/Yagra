// SPDX-License-Identifier: AGPL-3.0-only
// The node typeahead behind every picker: `NodePicker`, `GlobalSearch` and `ScopePicker`.
//
// Those three held byte-identical copies of this effect — same debounce, same `cancelled` flag,
// same empty-term-returns-the-first-page rule — differing only in *when* they were open and which
// loading setter they called. That is `extensibility.md` §3 exactly: three copies of a block whose
// only variable is a boolean, so a fix has to be applied three times to stay consistent, and the
// one that is missed is silent (a picker that overwrites a fresh result with a stale one still
// shows nodes).
//
// The settle is `useDebouncedValue`; what stays here is the part it explicitly does **not** do —
// a debounced *value* is not a debounced *request*, and responses can still arrive out of order
// however long you wait to send them.
//
// In a `.ts` (not `.tsx`) so a jsdom test can reach it, the way `pages/useFilterSearch.ts` is.

import { useEffect, useState } from 'react';
import { api } from '../services/api';
import { useDebouncedValue, SEARCH_DEBOUNCE_MS } from './useDebouncedValue';
import type { NodeSearchResult } from '../types/api';

export interface NodeSearch {
  /** The matches. Empty before the first answer — "no answer yet" is what `loading` says. */
  results: NodeSearchResult[];
  /** A search is pending: inside the debounce, or in flight. */
  loading: boolean;
}

/**
 * Server-side node search for a picker.
 *
 * `active` is the picker's own "am I open" condition (open, or open *and* on the Node tab). While
 * false nothing is fetched and the previous results are left alone, so reopening shows what was
 * there rather than blinking.
 *
 * An **empty term is a real query**, not a skipped one: it returns the first page by name so the
 * list is populated before the operator types. It settles with no delay, because there is nothing
 * to wait for — clearing the box is one keystroke, not a burst.
 */
export function useNodeSearch(active: boolean, query: string, max: number): NodeSearch {
  const [results, setResults] = useState<NodeSearchResult[]>([]);
  const [loading, setLoading] = useState(false);
  const term = query.trim();
  const settled = useDebouncedValue(term, term ? SEARCH_DEBOUNCE_MS : 0);

  // `loading` turns on as soon as the term changes, not when the request goes out: the operator is
  // typing and the list is already stale, so showing it as settled for 200ms would be a lie the
  // spinner exists to prevent.
  const pending = active && settled !== term;

  useEffect(() => {
    if (!active) return undefined;
    let cancelled = false;
    setLoading(true);
    api
      .searchNodes(settled, max)
      .then((r) => {
        if (!cancelled) setResults(r);
      })
      .catch(() => {
        // An empty list rather than the previous one: a picker that keeps showing matches for a
        // term whose search failed invites selecting a node that does not match what was typed.
        if (!cancelled) setResults([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      // The stale-response guard `useDebouncedValue` deliberately does not provide. Two requests
      // for two terms can answer in either order; without this, the slower earlier one wins.
      cancelled = true;
    };
  }, [active, settled, max]);

  return { results, loading: loading || pending };
}
