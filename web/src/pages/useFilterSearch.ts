// SPDX-License-Identifier: AGPL-3.0-only
// Filter mode's server-side node search for the inventory tree.
//
// Typing in the tree's Search box — or picking a state / kind / pool — does not filter a
// client-side copy of the fleet, because there isn't one (A-3: the tree paints from the group
// skeleton and loads members per group). It runs a debounced `/nodes?…` against the server and
// renders that one capped page of matches, merged with whatever the per-group member cache holds.
// Kept out of NodesPage because the pieces of state only make sense together, and because the page
// is a `.tsx`: the rule that decides WHEN the search is re-issued can only be tested from a `.ts`
// (see the test beside this file).

import { useCallback, useEffect, useState } from 'react';
import { api } from '../services/api';
import { SEARCH_DEBOUNCE_MS, useDebouncedValue } from '../lib/useDebouncedValue';
import { inventoryKey, inventoryQuery, type InventoryFilters } from './inventoryFilters';
import type { NodeSummary } from '../types/api';

/** Cap on filter-mode results — one server page of matches (the server's max). Beyond this the
 *  operator narrows the term; the fleet is never loaded into the browser. Exported because the
 *  page's truncation notice names the number. */
export const FILTER_SEARCH_LIMIT = 500;

export interface FilterSearch {
  /** The match page. Empty before the first answer; never null, since "no answer yet" is what
   *  `loading` says and every caller treated the two the same. */
  nodes: NodeSummary[];
  /** A search is pending — still inside the debounce, or in flight. */
  loading: boolean;
  /** The term the search was ISSUED for. Drives the group reveal (`revealedGroupKeys`), which is
   *  why it is published at issue time rather than on the answer: a folder matched by name loads
   *  its members in parallel with the search page instead of a round-trip behind it. */
  appliedTerm: string;
  /** Matches exist that this page does not contain.
   *
   *  ⚠️ **The server's word for it, not a length comparison.** With a state / kind / pool filter
   *  the server rejects candidates *after* the query, so a short page and a complete answer stop
   *  being the same thing: a filter that scans 5,000 rows and keeps 3 returns three rows and is
   *  still incomplete. Inferring truncation from `nodes.length` would report "complete" exactly
   *  when the answer is least trustworthy. */
  truncated: boolean;
  /** Re-issue the search for the current term. Stable identity, so a caller can hold it in a
   *  `useCallback`'s deps. Call it after any write that can change what matches — the search page
   *  is a cache like the per-group members are, and it has to be invalidated the same way. */
  refetch: () => void;
}

export function useFilterSearch(filter: string, filters: InventoryFilters): FilterSearch {
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [appliedTerm, setAppliedTerm] = useState('');
  const [truncated, setTruncated] = useState(false);
  /** Bumped by `refetch`. Its only job is to be in the effect's deps: asking for the SAME term
   *  again has to re-run the effect, and the term alone cannot say that. */
  const [nonce, setNonce] = useState(0);

  const refetch = useCallback(() => setNonce((v) => v + 1), []);

  // A plain string, not the filters object: it is re-derived from the URL every render, so putting
  // the object itself in the deps would re-issue the search on every unrelated re-render.
  const key = inventoryKey(filters);
  const typed = filter.trim();
  // The shared settle (`lib/useDebouncedValue`). Only the typed term needs it — the three selects
  // are discrete choices, and making an operator wait 200ms after picking one feels broken.
  //
  // Zero delay when the box is empty: clearing it is one keystroke rather than a burst, and the
  // operator who just cleared a filter is waiting to see the tree, not a spinner.
  const settled = useDebouncedValue(typed, typed ? SEARCH_DEBOUNCE_MS : 0);
  // Clearing the box takes effect **immediately**, without waiting for the settle: the previous
  // term is still the settled value for one more tick, and searching for it after the operator has
  // emptied the box means one pointless request and a spinner over a tree that is about to stop
  // being filtered at all.
  const term = typed ? settled : '';
  const query = inventoryQuery(filters);
  const narrowed = Boolean(query.state || query.kind || query.pool);
  // Typing has already invalidated what is on screen, so report `loading` from the keystroke
  // rather than from the moment the request goes out — but only when a search is actually coming.
  // Without that condition, clearing the last filter would show a spinner over a tree that is not
  // going to be re-fetched at all.
  const settling = (Boolean(typed) || narrowed) && typed !== settled;

  useEffect(() => {
    if (!term && !narrowed) {
      // Clearing everything must un-reveal the folders the last term opened, not freeze them —
      // and must clear `loading`, which the cancelled request below will not do for itself.
      setAppliedTerm('');
      setLoading(false);
      setTruncated(false);
      return undefined;
    }
    let cancelled = false;
    setLoading(true);
    setAppliedTerm(term);
    api
      .listNodesPage({ search: term, limit: FILTER_SEARCH_LIMIT, ...query })
      .then((page) => {
        // The previous page stays on screen until this one lands. A re-fetch is therefore
        // invisible unless something changed, which is the point: a write while a filter is
        // active must not blink the tree to empty and back.
        if (!cancelled) {
          setNodes(page.nodes);
          setTruncated(page.truncated);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setNodes([]);
          setTruncated(false);
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      // The stale-response guard the settle deliberately does not provide: two requests for two
      // terms can answer in either order, and without this the slower earlier one wins.
      cancelled = true;
    };
    // `filters` is deliberately absent: `key` is its value identity (see above), and the effect
    // reads the object only to build the query it fires with.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [term, key, nonce]);

  return { nodes, loading: loading || settling, appliedTerm, truncated, refetch };
}
