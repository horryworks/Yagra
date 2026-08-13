// SPDX-License-Identifier: AGPL-3.0-only
// The one debounce. Before this, the same `setTimeout` + cleanup was hand-written in eight places
// (the column filter row, MibRepositoryPage, NodesPage, NodePicker, GlobalSearch, ScopePicker, FlowTab,
// CollectionEditor) — and the filter work was about to add twenty more.
//
// Deliberately holds no judgement, because nothing can test it: Vitest runs `environment: 'node'`
// (testing.md), so `renderHook` is unavailable and a `.tsx` test is a file nothing runs. Everything
// that decides *what is asked for* belongs in a plain `.ts` beside the screen — see
// `lib/filterQuery.ts` and `troubleshoot/findingsQuery.ts`. Keep this file boring enough that its
// untestability costs nothing.

import { useEffect, useState } from 'react';

/** The settle delay for a filter/search box. `design-system.md` §4.1 calls the search box
 *  "instant"; 200ms is what every implementation in the tree actually used. */
export const SEARCH_DEBOUNCE_MS = 200;

/**
 * `value` once it has stopped changing for `ms`.
 *
 * `ms` is a parameter rather than a fixed constant so a caller can collapse the tree's *other*
 * debounce variant — "settle immediately when the box is cleared" — into this same hook:
 *
 * ```ts
 * const settled = useDebouncedValue(q, q.trim() ? SEARCH_DEBOUNCE_MS : 0);
 * ```
 *
 * The effect depends on both, so changing the delay re-arms the timer. `ms = 0` settles on the next
 * tick rather than synchronously — that keeps one code path instead of two, and a tick is below the
 * threshold the "instant" rule is about.
 *
 * ⚠️ This debounces a *value*, not a *request*. A caller that fetches on the settled value still
 * needs its own stale-response guard (the `cancelled` flag in `NodePicker` / `GlobalSearch` /
 * `ScopePicker`) — responses can arrive out of order however long you wait to send them.
 */
export function useDebouncedValue<T>(value: T, ms = SEARCH_DEBOUNCE_MS): T {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    const id = setTimeout(() => setSettled(value), ms);
    return () => clearTimeout(id);
  }, [value, ms]);
  return settled;
}
