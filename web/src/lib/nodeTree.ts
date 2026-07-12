// Pure helpers for the inventory tree: build a nested group/node structure from the flat API
// shapes, answer "is X a descendant of Y" for drag-drop cycle guards, and roll up the health of a
// group's descendant nodes. Kept free of React so they can be unit-tested directly.

import type { NodeGroup, NodeState, NodeSummary } from '../types/api';

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
 *  (rollup from loaded descendants, every group treated as loaded). */
export function flattenTree(
  tree: NodeTreeData,
  opts: {
    collapsed: Record<string, boolean>;
    filter: string;
    groupCounts?: Record<string, StateCounts>;
    loadedGroups?: Set<string>;
  },
): FlatRow[] {
  const q = opts.filter.trim().toLowerCase();
  const filtering = q.length > 0;
  const rows: FlatRow[] = [];
  const counts = opts.groupCounts;
  // Per-group subtree tally from the server direct counts (bottom-up over the built, acyclic tree).
  const subtree = counts ? subtreeTallyMap(tree.roots, counts) : null;
  // Filter mode full-loads the inventory, so every group's members are present — no loading rows.
  const isLoaded = (id: string) => filtering || !opts.loadedGroups || opts.loadedGroups.has(id);

  const walkGroup = (group: TreeGroup, depth: number, ancestorMatch: boolean): void => {
    const selfMatch = filtering && group.name.toLowerCase().includes(q);
    const effMatch = ancestorMatch || selfMatch;
    // Hide a group entirely when filtering and nothing under it matches.
    if (filtering && !effMatch && !subtreeMatches(group, q)) return;

    const isOpen = filtering ? true : !opts.collapsed[group.id];
    const tally = subtree ? subtree.get(group.id) ?? tallyFromCounts(emptyCounts())
                          : tallyStates(descendantNodes(group));
    const directTotal = counts ? countsTotal(counts[group.id] ?? emptyCounts()) : group.nodes.length;
    // A twisty is offered when the group has sub-groups or any (counted or loaded) member below it.
    const hasChildren = group.children.length > 0 || tally.total > 0;
    rows.push({ kind: 'group', depth, group, isOpen, hasChildren, tally });
    if (!isOpen) return;
    // Children first, then this group's own member nodes — matching the recursive render order.
    for (const child of group.children) walkGroup(child, depth + 1, effMatch);
    if (!isLoaded(group.id)) {
      // Members not fetched yet: a single loading placeholder when the group actually has some.
      if (directTotal > 0) rows.push({ kind: 'group-loading', depth: depth + 1, groupId: group.id });
      return;
    }
    for (const n of group.nodes) {
      if (!filtering || effMatch || n.name.toLowerCase().includes(q)) {
        rows.push({ kind: 'node', depth: depth + 1, node: n });
      }
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

/** The order states are shown in a health bar / legend (best → worst, with the neutral states
 *  trailing). Stable so the bar segments and legend read consistently everywhere. */
export const STATE_ORDER: NodeState[] = [
  'ok',
  'warning',
  'critical',
  'unreachable',
  'maintenance',
  'unknown',
];

/** States that mean a node "needs attention" (surfaced in red counts on group rollups). */
const PROBLEM_STATES = new Set<NodeState>(['warning', 'critical', 'unreachable']);

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
  const counts: Record<NodeState, number> = {
    ok: 0,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
  };
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

/** A zero-filled per-state count object. */
function emptyCounts(): StateCounts {
  return { ok: 0, warning: 0, critical: 0, unreachable: 0, maintenance: 0, unknown: 0 };
}

/** Sum of every state count in a per-state tally. */
export function countsTotal(c: StateCounts): number {
  return STATE_ORDER.reduce((n, s) => n + (c[s] ?? 0), 0);
}

/** Build a `StateTally` (counts + total + need-attention) from a raw per-state count object — the
 *  counts-driven twin of {@link tallyStates}, for the server-side per-group rollup (A-1/A-3). */
export function tallyFromCounts(counts: StateCounts): StateTally {
  let total = 0;
  let needAttention = 0;
  for (const s of STATE_ORDER) {
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
    const acc = emptyCounts();
    const own = counts[g.id];
    if (own) for (const s of STATE_ORDER) acc[s] += own[s] ?? 0;
    for (const child of g.children) {
      const sub = visit(child);
      for (const s of STATE_ORDER) acc[s] += sub[s];
    }
    out.set(g.id, tallyFromCounts(acc));
    return acc;
  };
  for (const r of roots) visit(r);
  return out;
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
    const k = g.parent_id;
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
