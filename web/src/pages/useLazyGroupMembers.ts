// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's per-group member cache (A-3 lazy load).
//
// The tree paints from the group skeleton plus the server's per-group health rollup, so it is
// instant at any fleet size; a group's member nodes are fetched only once that group's contents are
// actually on screen. That policy needs four pieces of state that only make sense together — what
// is loaded, what is in flight, what came back truncated, and the members themselves — and three
// effects that must not race each other into fetching the same group twice. Kept out of NodesPage
// so the page reads as a page, and so "which groups do we want loaded" stays one question with three
// answers (visible-and-open, the selected group's subtree, and the subtree a filter revealed) rather
// than three tangled effects.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { api } from '../services/api';
import { revealedGroupKeys, subtreeGroupIds, UNGROUPED, visibleOpenGroupKeys } from '../lib/nodeTree';
import type { NodeGroup, NodeSummary } from '../types/api';

/** Cap on how many groups one filter term may reveal. A one-letter term matches most folders in a
 *  large fleet and each key is one `/nodes/by-group` request — the fan-out this lazy tree exists to
 *  avoid, and here it hangs off a text input. */
const REVEAL_GROUP_CAP = 200;

export interface LazyGroupMembers {
  /** Every member loaded so far, flattened — what the tree renders in browse mode. */
  nodes: NodeSummary[];
  /** Which groups have been fetched. The tree shows a placeholder row under the others. */
  loadedGroups: Set<string>;
  /** The groups the active filter revealed (see `revealedGroupKeys`). Returned rather than derived
   *  a second time by the caller, so the set that gets FETCHED and the set the tree draws loading
   *  rows for cannot disagree. Empty while browsing. */
  revealedGroups: Set<string>;
  /** The term matched more groups than the cap, so some matched folders show no members. */
  revealTruncated: boolean;
  /** Whether any group's members came back capped by the server (the page says so). */
  anyTruncated: boolean;
  /** Drop everything so open groups re-fetch. Call after any write that can change membership. */
  invalidate: () => void;
}

export function useLazyGroupMembers(opts: {
  groups: NodeGroup[];
  /** The user's collapsed-group prefs — which groups are open decides what is worth fetching. */
  collapsed: Record<string, boolean>;
  /** False while the group skeleton is still loading: there is no tree to walk yet. */
  ready: boolean;
  /** False in filter mode, where a server-side search owns the tree instead of the cache. */
  browsing: boolean;
  /** The selected group, if any. Its whole subtree loads so the detail pane can roll it up —
   *  independent of `browsing`, since a group stays selected while the operator types a filter. */
  selectedGroupId: string | null;
  /** The APPLIED (debounced) inventory filter. A term matching a group's NAME reveals that group's
   *  whole subtree: its members load so the operator sees the folder's contents rather than an empty
   *  folder. Loaded independently of `browsing`, like the selected subtree — filter mode is exactly
   *  when it matters, because the server search page only carries nodes that matched. A plain string
   *  (not a precomputed array) so a re-render cannot churn the effect on identity alone. */
  filterTerm: string;
}): LazyGroupMembers {
  const { groups, collapsed, ready, browsing, selectedGroupId, filterTerm } = opts;

  const [loadedNodes, setLoadedNodes] = useState<Record<string, NodeSummary[]>>({});
  const [loadedGroups, setLoadedGroups] = useState<Set<string>>(new Set());
  const [loadingGroups, setLoadingGroups] = useState<Set<string>>(new Set());
  const [truncatedGroups, setTruncatedGroups] = useState<Set<string>>(new Set());

  /** Fetch one group's (or the ungrouped bucket's) direct members. The in-flight set is what stops
   *  the two effects below from both firing for the same key on the same render. */
  const load = useCallback((key: string) => {
    setLoadingGroups((prev) => {
      const next = new Set(prev);
      next.add(key);
      return next;
    });
    api
      .getGroupNodes(key === UNGROUPED ? null : key)
      .then((res) => {
        setLoadedNodes((prev) => ({ ...prev, [key]: res.nodes }));
        setLoadedGroups((prev) => new Set(prev).add(key));
        if (res.truncated) setTruncatedGroups((prev) => new Set(prev).add(key));
      })
      .catch(() => {
        /* leave unloaded — retried when the group is next opened/selected */
      })
      .finally(() => {
        setLoadingGroups((prev) => {
          const next = new Set(prev);
          next.delete(key);
          return next;
        });
      });
  }, []);

  const loadMissing = useCallback(
    (keys: string[]) => {
      for (const key of keys) {
        if (!loadedGroups.has(key) && !loadingGroups.has(key)) load(key);
      }
    },
    [loadedGroups, loadingGroups, load],
  );

  // Browse mode: every open + visible group (and the ungrouped bucket). Re-runs as the operator
  // expands groups (the collapsed prefs change) or after an invalidate.
  useEffect(() => {
    if (!ready || !browsing) return;
    loadMissing(visibleOpenGroupKeys(groups, collapsed));
  }, [ready, browsing, groups, collapsed, loadMissing]);

  // A selected group's whole subtree, so the detail pane's rollup counts its descendants' members.
  useEffect(() => {
    if (!ready || !selectedGroupId) return;
    loadMissing(subtreeGroupIds(groups, selectedGroupId));
  }, [ready, selectedGroupId, groups, loadMissing]);

  const revealedGroups = useMemo(
    () => new Set(revealedGroupKeys(groups, filterTerm, REVEAL_GROUP_CAP)),
    [groups, filterTerm],
  );

  // The subtree a filter revealed by matching a group's own name. Independent of `browsing` for the
  // same reason as the selected subtree: filter mode is when it is needed. The in-flight set is what
  // keeps this from racing the other two into a duplicate request per group.
  useEffect(() => {
    if (!ready || revealedGroups.size === 0) return;
    loadMissing([...revealedGroups]);
  }, [ready, revealedGroups, loadMissing]);

  const invalidate = useCallback(() => {
    setLoadedNodes({});
    setLoadedGroups(new Set());
    setLoadingGroups(new Set());
    setTruncatedGroups(new Set());
  }, []);

  const nodes = useMemo(() => Object.values(loadedNodes).flat(), [loadedNodes]);

  return {
    nodes,
    loadedGroups,
    revealedGroups,
    revealTruncated: revealedGroups.size >= REVEAL_GROUP_CAP,
    anyTruncated: truncatedGroups.size > 0,
    invalidate,
  };
}
