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
