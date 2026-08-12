// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the inventory tree: build a nested group/node structure from the flat API
// shapes, answer "is X a descendant of Y" for drag-drop cycle guards, and roll up the health of a
// group's descendant nodes. Kept free of React so they can be unit-tested directly.

import { GROUP_TYPES } from '../types/api';
import type { GroupType, NodeGroup, NodeState, NodeSummary } from '../types/api';
import { DISPLAY_ORDER, PROBLEM_STATES, emptyStateCounts } from './nodeState';

/** Re-exported for the health-bar/legend call sites that read "the order states are shown in".
 *  The definition lives in `nodeState.ts` with the rest of the NodeState vocabulary. */
export { DISPLAY_ORDER as STATE_ORDER } from './nodeState';

/** A group with its child groups and member nodes resolved (built from the flat lists). */
export interface TreeGroup extends NodeGroup {
  children: TreeGroup[];
  nodes: NodeSummary[];
}

/** The assembled tree: top-level groups + the nodes that belong to no group. */
export interface NodeTreeData {
  roots: TreeGroup[];
  ungrouped: NodeSummary[];
}

/** Order siblings by their manual `sort_order` (drag-reorder), falling back to name so equal or
 *  unset orders stay stable. */
const byOrder = <T extends { sort_order: number; name: string }>(a: T, b: T) =>
  a.sort_order - b.sort_order || a.name.localeCompare(b.name);

/** Build the nested tree from the flat group + node lists. Nodes whose `group_id` is null or
 *  points at an unknown group fall into `ungrouped`; groups whose `parent_id` is unknown are
 *  treated as top-level (so a stale reference can never hide a row). Children and nodes are
 *  ordered by their manual `sort_order` (then name) for stable display. */
export function buildNodeTree(groups: NodeGroup[], nodes: NodeSummary[]): NodeTreeData {
  const byId = new Map<string, TreeGroup>();
  for (const g of groups) byId.set(g.id, { ...g, children: [], nodes: [] });

  const roots: TreeGroup[] = [];
  for (const g of byId.values()) {
    const parent = g.parent_id ? byId.get(g.parent_id) : undefined;
    if (parent) parent.children.push(g);
    else roots.push(g);
  }

  const ungrouped: NodeSummary[] = [];
  for (const n of nodes) {
    const group = n.group_id ? byId.get(n.group_id) : undefined;
    if (group) group.nodes.push(n);
    else ungrouped.push(n);
  }

  for (const g of byId.values()) {
    g.children.sort(byOrder);
    g.nodes.sort(byOrder);
  }
  roots.sort(byOrder);
  ungrouped.sort(byOrder);
  return { roots, ungrouped };
}

/** Every member node at or below a group in the tree — the group's own nodes plus those of all
 *  descendant groups (recursively). Used for the per-group health rollup and member counts. The
 *  passed `group` is a built `TreeGroup` (children + nodes already resolved). */
export function descendantNodes(group: TreeGroup): NodeSummary[] {
  const out: NodeSummary[] = [...group.nodes];
  for (const child of group.children) out.push(...descendantNodes(child));
  return out;
}

/** One visible row of the inventory tree, flattened for virtualized rendering (S13). The tree shape
 *  is carried by row order + `depth` (indentation is purely `depth × INDENT`), so a windowed list of
 *  these renders identically to the old recursive DOM but only builds the on-screen rows. */
export type FlatRow =
  | {
      kind: 'group';
      depth: number;
      group: TreeGroup;
      isOpen: boolean;
      hasChildren: boolean;
      /** Rolled-up health of the group's whole subtree — from the server per-group counts when
       *  supplied (A-3 lazy load, correct over the whole fleet even before members load), else from
       *  the loaded descendant members. Drives the row's health bar + member count. */
      tally: StateTally;
    }
  | { kind: 'node'; depth: number; node: NodeSummary }
  /** Placeholder under an open group whose members haven't been lazily fetched yet (A-3). */
  | { kind: 'group-loading'; depth: number; groupId: string }
  | { kind: 'ungrouped-head'; count: number }
  | { kind: 'ungrouped-node'; depth: number; node: NodeSummary };

/** A stable key for a flat row (for React keys + virtualizer identity). */
export function flatRowKey(row: FlatRow): string {
  switch (row.kind) {
    case 'group':
      return `g:${row.group.id}`;
    case 'node':
    case 'ungrouped-node':
      return `n:${row.node.id}`;
    case 'group-loading':
      return `loading:${row.groupId}`;
    case 'ungrouped-head':
      return 'ungrouped-head';
  }
}

/** The inventory filter's comparison form: trimmed and lower-cased; empty means "not filtering".
 *
 *  One spelling on purpose. The rule that decides which groups are REVEALED
 *  ({@link revealedGroupKeys}, which drives what the page fetches) and the rule that decides what is
 *  SHOWN ({@link flattenTree}) have to agree — a trailing space normalized in one and not the other
 *  is a group that renders as matched and never loads its members. */
export function filterTerm(filter: string): string {
  return filter.trim().toLowerCase();
}

/** Concatenate node lists, keeping the FIRST entry for a repeated id.
 *
 *  Filter mode renders the server search page PLUS the members of the groups the term revealed, and
 *  a node is routinely in both lists. A duplicate is not a harmless extra row: `flatRowKey` is
 *  `n:<id>`, so it collides in the React key and in the virtualizer's `getItemKey`. */
export function mergeNodesById(...lists: NodeSummary[][]): NodeSummary[] {
  const seen = new Set<string>();
  const out: NodeSummary[] = [];
  for (const list of lists) {
    for (const n of list) {
      if (seen.has(n.id)) continue;
      seen.add(n.id);
      out.push(n);
    }
  }
  return out;
}

/** The group keys an active filter REVEALS: every group whose own name matches the term, plus that
 *  group's whole subtree.
 *
 *  A group matched by name shows its members even though none of them matched — the operator
 *  searched for the folder, so the folder's contents are the answer. Those members have to be
 *  fetched per group: filter mode's server search (`/nodes?search=`) matches a node's name/address
 *  and knows nothing about groups, so a matched folder's members are simply not in its answer.
 *
 *  Never includes {@link UNGROUPED} — nothing can match the bucket's name. Deterministic (the API
 *  returns groups ordered by sort_order, name, id) and CAPPED: a one-letter term matches most of a
 *  fleet's folders, and each key is one `/nodes/by-group` request, so this is the one place a
 *  keystroke can fan out into N requests — the fan-out the lazy tree exists to avoid. Past the cap a
 *  group degrades to the old behaviour (its matching nodes still show, its other members do not),
 *  never to a loading placeholder that nothing will ever resolve. */
export function revealedGroupKeys(groups: NodeGroup[], filter: string, cap: number): string[] {
  const q = filterTerm(filter);
  if (!q) return [];
  const out: string[] = [];
  const seen = new Set<string>();
  for (const g of groups) {
    if (!g.name.toLowerCase().includes(q)) continue;
    for (const id of subtreeGroupIds(groups, g.id)) {
      if (seen.has(id)) continue;
      seen.add(id);
      out.push(id);
      if (out.length >= cap) return out;
    }
  }
  return out;
}

/** Whether a group's subtree contains anything matching `q` (its own name, a descendant group's
 *  name, or a member node's name) — so ancestor groups stay visible to reveal a nested match. */
function subtreeMatches(group: TreeGroup, q: string): boolean {
  if (group.name.toLowerCase().includes(q)) return true;
  if (group.nodes.some((n) => n.name.toLowerCase().includes(q))) return true;
  return group.children.some((c) => subtreeMatches(c, q));
}

/** Flatten the visible rows of the inventory tree in display order, honouring collapse state and
 *  the name filter — the single source of truth the virtualized `NodeTree` renders. Collapsed
 *  groups omit their descendants; while filtering, every group is force-expanded and non-matching
 *  rows are hidden. Pure (no React) so the ordering/visibility rules are unit-tested directly.
 *
 *  Lazy load (A-3): when `groupCounts` (server per-group direct counts) is supplied, group rows roll
 *  up from those — correct over the whole fleet even before members are fetched. `loadedGroups` says
 *  which groups' members have been fetched; an open group that isn't loaded yet emits a single
 *  `group-loading` placeholder instead of its members. Omit both for the legacy full-node path
 *  (rollup from loaded descendants, every group treated as loaded).
 *
 *  `revealedGroups` ({@link revealedGroupKeys}) is filter mode's counterpart: the groups whose whole
 *  membership is being fetched because the term matched the group's own name. Only those can be
 *  "still loading" while filtering — every other group is showing the search page's hits and nothing
 *  more, so a group past the reveal cap gets no members rather than a placeholder that never
 *  resolves. */
export function flattenTree(
  tree: NodeTreeData,
  opts: {
    collapsed: Record<string, boolean>;
    filter: string;
    groupCounts?: Record<string, StateCounts>;
    loadedGroups?: Set<string>;
    revealedGroups?: Set<string>;
  },
): FlatRow[] {
  const q = filterTerm(opts.filter);
  const filtering = q.length > 0;
  const rows: FlatRow[] = [];
  const counts = opts.groupCounts;
  // Per-group subtree tally from the server direct counts (bottom-up over the built, acyclic tree).
  const subtree = counts ? subtreeTallyMap(tree.roots, counts) : null;
  // Browsing: a group whose members haven't been fetched stands in with a placeholder. Filtering:
  // the server search page carries every match there is, so only a REVEALED group (whose members are
  // being fetched separately, because the term matched the folder rather than its contents) can be
  // waiting on anything.
  const isLoaded = (id: string) =>
    filtering
      ? !opts.revealedGroups?.has(id) || (opts.loadedGroups?.has(id) ?? true)
      : !opts.loadedGroups || opts.loadedGroups.has(id);

  const walkGroup = (group: TreeGroup, depth: number, ancestorMatch: boolean): void => {
    const selfMatch = filtering && group.name.toLowerCase().includes(q);
    const effMatch = ancestorMatch || selfMatch;
    // Hide a group entirely when filtering and nothing under it matches.
    if (filtering && !effMatch && !subtreeMatches(group, q)) return;

    const isOpen = filtering ? true : !opts.collapsed[group.id];
    const tally = subtree ? subtree.get(group.id) ?? tallyFromCounts(emptyStateCounts())
                          : tallyStates(descendantNodes(group));
    const directTotal = counts ? countsTotal(counts[group.id] ?? emptyStateCounts()) : group.nodes.length;
    // A twisty is offered when the group has sub-groups or any (counted or loaded) member below it.
    const hasChildren = group.children.length > 0 || tally.total > 0;
    rows.push({ kind: 'group', depth, group, isOpen, hasChildren, tally });
    if (!isOpen) return;
    // Children first, then this group's own member nodes — matching the recursive render order.
    for (const child of group.children) walkGroup(child, depth + 1, effMatch);
    const shown = group.nodes.filter(
      (n) => !filtering || effMatch || n.name.toLowerCase().includes(q),
    );
    for (const n of shown) rows.push({ kind: 'node', depth: depth + 1, node: n });
    // Members still arriving: one placeholder standing in for the rest. What we already have goes
    // first — in filter mode the search page already carries this group's MATCHING nodes, and hiding
    // them behind the placeholder would flicker them out while the rest of the folder loads.
    if (!isLoaded(group.id) && directTotal > shown.length) {
      rows.push({ kind: 'group-loading', depth: depth + 1, groupId: group.id });
    }
  };

  for (const g of tree.roots) walkGroup(g, 0, false);

  const ungroupedShown = filtering
    ? tree.ungrouped.filter((n) => n.name.toLowerCase().includes(q))
    : tree.ungrouped;
  // Show the ungrouped header + its root drop zone whenever there's any inventory (so the drop zone
  // is reachable next to the groups), but not when filtering yields no ungrouped matches, and not
  // for a completely empty inventory (the page shows its own empty-state message instead).
  const showUngrouped = filtering
    ? ungroupedShown.length > 0
    : tree.roots.length > 0 || tree.ungrouped.length > 0;
  if (showUngrouped) {
    rows.push({ kind: 'ungrouped-head', count: tree.ungrouped.length });
    for (const n of ungroupedShown) rows.push({ kind: 'ungrouped-node', depth: 1, node: n });
  }
  return rows;
}

/** A per-state tally of a node set, plus how many of them need attention. Counts cover every
 *  `NodeState`, so a missing state is simply `0` (handy for proportional bar widths). */
export interface StateTally {
  counts: Record<NodeState, number>;
  total: number;
  needAttention: number;
}

/** Count a node set by state. Drives the health bar segment widths, the per-state legend, and the
 *  "N need attention" summary on group rollups and the page header. */
export function tallyStates(nodes: NodeSummary[]): StateTally {
  const counts = emptyStateCounts();
  let needAttention = 0;
  for (const n of nodes) {
    counts[n.state] += 1;
    if (PROBLEM_STATES.has(n.state)) needAttention += 1;
  }
  return { counts, total: nodes.length, needAttention };
}

/** A raw per-state count object — the `/fleet/group-summary` value shape (server per-group rollup,
 *  A-1/A-3). Structurally identical to `StateTally.counts`. */
export type StateCounts = Record<NodeState, number>;

/** Sum of every state count in a per-state tally. */
export function countsTotal(c: StateCounts): number {
  return DISPLAY_ORDER.reduce((n, s) => n + (c[s] ?? 0), 0);
}

/** Build a `StateTally` (counts + total + need-attention) from a raw per-state count object — the
 *  counts-driven twin of {@link tallyStates}, for the server-side per-group rollup (A-1/A-3). */
export function tallyFromCounts(counts: StateCounts): StateTally {
  let total = 0;
  let needAttention = 0;
  for (const s of DISPLAY_ORDER) {
    const n = counts[s] ?? 0;
    total += n;
    if (PROBLEM_STATES.has(s)) needAttention += n;
  }
  return { counts, total, needAttention };
}

/** Roll each group's DIRECT member counts up its subtree, yielding a per-group DESCENDANT tally
 *  (the whole subtree's health) from the server per-group direct counts (A-3). Bottom-up over the
 *  built tree, which is acyclic (each group appears once), so no cycle guard is needed. */
function subtreeTallyMap(
  roots: TreeGroup[],
  counts: Record<string, StateCounts>,
): Map<string, StateTally> {
  const out = new Map<string, StateTally>();
  const visit = (g: TreeGroup): StateCounts => {
    const acc = emptyStateCounts();
    const own = counts[g.id];
    if (own) for (const s of DISPLAY_ORDER) acc[s] += own[s] ?? 0;
    for (const child of g.children) {
      const sub = visit(child);
      for (const s of DISPLAY_ORDER) acc[s] += sub[s];
    }
    out.set(g.id, tallyFromCounts(acc));
    return acc;
  };
  for (const r of roots) visit(r);
  return out;
}

/** Read a group's `group_type` off the wire, where it is a bare string (the server validates it at
 *  the write edge, so the closed set only exists in TypeScript — see `GROUP_TYPES`). Anything
 *  unrecognised reads as `generic`, which is the plain-folder rendering the icon already fell back
 *  to. The single narrowing site, so the picker, the tree and the detail pane cannot disagree. */
export function asGroupType(value: string | undefined): GroupType {
  return GROUP_TYPES.find((g) => g === value) ?? 'generic';
}

/** The chain of group names from the top-level ancestor down to `groupId` (inclusive), for the
 *  detail-pane breadcrumb eyebrow (e.g. `Tokyo / Edge / Firewall`). Empty for a null/unknown id.
 *  Bounded by the group count so malformed (cyclic) data can't loop forever. */
export function groupPath(groups: NodeGroup[], groupId: string | null): string[] {
  if (!groupId) return [];
  const byId = new Map(groups.map((g) => [g.id, g]));
  const out: string[] = [];
  let cur = byId.get(groupId);
  for (let i = 0; cur && i <= groups.length; i++) {
    out.unshift(cur.name);
    cur = cur.parent_id ? byId.get(cur.parent_id) : undefined;
  }
  return out;
}

/** Flatten the group hierarchy into depth-indented `{ id, label }` options for a `<select>`, so
 *  the tree shape reads in a flat list (used by the add/edit-group and move-node pickers). */
export function groupOptions(groups: NodeGroup[]): { id: string; label: string }[] {
  const byParent = new Map<string | null, NodeGroup[]>();
  for (const g of groups) {
    const k = g.parent_id ?? null;
    byParent.set(k, [...(byParent.get(k) ?? []), g]);
  }
  const out: { id: string; label: string }[] = [];
  const walk = (parent: string | null, depth: number) => {
    for (const g of (byParent.get(parent) ?? []).sort((a, b) => a.name.localeCompare(b.name))) {
      out.push({ id: g.id, label: `${'  '.repeat(depth)}${g.name}` });
      walk(g.id, depth + 1);
    }
  };
  walk(null, 0);
  return out;
}

/** Sentinel key for the ungrouped bucket in the per-group member cache and the `/nodes/by-group`
 *  call (which takes `null` for it). A sentinel rather than `null` so the cache can stay a plain
 *  `Record<string, …>` keyed the same way for both. */
export const UNGROUPED = '__ungrouped__';

/** The group keys whose direct members should be loaded now: the ungrouped bucket (always) plus
 *  every group that is open AND visible — i.e. every one of its ancestors is also open, so its
 *  expanded content is actually on screen (A-3 lazy load).
 *
 *  The ancestor condition is the whole point. A group the operator once expanded stays open in the
 *  collapse prefs forever, so "open" alone is a set that only grows; without the visibility test
 *  the first render of a deep tree would fetch members for every group ever expanded — the fleet
 *  load this lazy path exists to avoid, and invisible from the screen it produces. */
export function visibleOpenGroupKeys(
  groups: NodeGroup[],
  collapsed: Record<string, boolean>,
): string[] {
  const childrenOf = new Map<string | null, NodeGroup[]>();
  for (const g of groups) {
    const k = g.parent_id ?? null;
    childrenOf.set(k, [...(childrenOf.get(k) ?? []), g]);
  }
  const out: string[] = [UNGROUPED];
  const walk = (parentId: string | null, ancestorsOpen: boolean) => {
    for (const g of childrenOf.get(parentId) ?? []) {
      const open = !collapsed[g.id];
      if (ancestorsOpen && open) out.push(g.id);
      walk(g.id, ancestorsOpen && open);
    }
  };
  walk(null, true);
  return out;
}

/** A group id plus every descendant group id (its whole subtree). Used to lazily load a selected
 *  group's subtree so the detail pane can roll up its members. Cycle-guarded by the visited set —
 *  this walks the raw `parent_id` edges from the API, not the built (acyclic) tree. */
export function subtreeGroupIds(groups: NodeGroup[], rootId: string): string[] {
  const childrenOf = new Map<string, NodeGroup[]>();
  for (const g of groups) {
    if (g.parent_id) childrenOf.set(g.parent_id, [...(childrenOf.get(g.parent_id) ?? []), g]);
  }
  const out: string[] = [];
  const seen = new Set<string>();
  const walk = (id: string) => {
    if (seen.has(id)) return;
    seen.add(id);
    out.push(id);
    for (const c of childrenOf.get(id) ?? []) walk(c.id);
  };
  walk(rootId);
  return out;
}

/** Whether `candidateId` is `ancestorId` itself or sits anywhere below it in the group tree.
 *  Used to forbid moving a group into its own subtree (which would create a cycle). Bounded by
 *  the group count so malformed (already-cyclic) data can't loop forever. */
export function isSelfOrDescendant(
  groups: NodeGroup[],
  ancestorId: string,
  candidateId: string,
): boolean {
  if (ancestorId === candidateId) return true;
  const parentOf = new Map(groups.map((g) => [g.id, g.parent_id]));
  let cur: string | null | undefined = candidateId;
  for (let i = 0; i <= groups.length; i++) {
    if (cur == null) return false;
    if (cur === ancestorId) return true;
    cur = parentOf.get(cur) ?? null;
  }
  return true;
}

/** Whether a filter-mode result set hit the server's cap, so matches are missing from the list.
 *
 *  Filter mode asks the server for one page of matches. A fleet with more matches than the cap
 *  gets a silently short list: the group-truncation notice does not cover this path, because that
 *  one reports per-group member caps while filtering bypasses groups entirely. Without this the
 *  operator reads "these are the switches" when it is "these are the first N switches". */
export function filterResultsTruncated(
  filtering: boolean,
  resultCount: number,
  cap: number,
): boolean {
  return filtering && resultCount >= cap;
}
