// SPDX-License-Identifier: AGPL-3.0-only
// Filter mode's server-side node search for the inventory tree.
//
// Typing in the tree's Search box does not filter a client-side copy of the fleet — there isn't
// one (A-3: the tree paints from the group skeleton and loads members per group). It runs a
// debounced `/nodes?search=` against the server and renders that one capped page of matches,
// merged with whatever the per-group member cache holds. Kept out of NodesPage because the four
// pieces of state only make sense together, and because the page is a `.tsx`: the rule that decides
// WHEN the search is re-issued can only be tested from a `.ts` (see the test beside this file).

import { useCallback, useEffect, useState } from 'react';
import { api } from '../services/api';
import { filterResultsTruncated } from '../lib/nodeTree';
import type { NodeSummary } from '../types/api';

/** Cap on filter-mode results — one server page of matches (the server's max). Beyond this the
 *  operator narrows the term; the fleet is never loaded into the browser. Exported because the
 *  page's truncation notice names the number. */
export const FILTER_SEARCH_LIMIT = 500;
/** Debounce so a fast typist fires one request, not one per keystroke. */
const FILTER_DEBOUNCE_MS = 200;

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
  /** The page came back at the server's cap, so matches are missing from it. */
  truncated: boolean;
  /** Re-issue the search for the current term. Stable identity, so a caller can hold it in a
   *  `useCallback`'s deps. Call it after any write that can change what matches — the search page
   *  is a cache like the per-group members are, and it has to be invalidated the same way. */
  refetch: () => void;
}

export function useFilterSearch(filter: string): FilterSearch {
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [loading, setLoading] = useState(false);
  const [appliedTerm, setAppliedTerm] = useState('');
  /** Bumped by `refetch`. Its only job is to be in the effect's deps: asking for the SAME term
   *  again has to re-run the effect, and the term alone cannot say that. */
  const [nonce, setNonce] = useState(0);

  const refetch = useCallback(() => setNonce((v) => v + 1), []);

  useEffect(() => {
    const term = filter.trim();
    if (!term) {
      // Clearing the box must un-reveal the folders the last term opened, not freeze them — and
      // must clear `loading`, which the cancelled request below will not do for itself.
      setAppliedTerm('');
      setLoading(false);
      return undefined;
    }
    let cancelled = false;
    setLoading(true);
    const h = setTimeout(() => {
      setAppliedTerm(term);
      api
        .listNodesPage({ search: term, limit: FILTER_SEARCH_LIMIT })
        .then((page) => {
          // The previous page stays on screen until this one lands. A re-fetch is therefore
          // invisible unless something changed, which is the point: a write while a filter is
          // active must not blink the tree to empty and back.
          if (!cancelled) setNodes(page.nodes);
        })
        .catch(() => {
          if (!cancelled) setNodes([]);
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    }, FILTER_DEBOUNCE_MS);
    return () => {
      // A response for a superseded term (or a term the operator has since cleared) is dropped.
      cancelled = true;
      clearTimeout(h);
    };
  }, [filter, nonce]);

  return {
    nodes,
    loading,
    appliedTerm,
    truncated: filterResultsTruncated(appliedTerm.length > 0, nodes.length, FILTER_SEARCH_LIMIT),
    refetch,
  };
}
