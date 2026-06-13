// Pure helpers for the inventory tree: build a nested group/node structure from the flat API
// shapes, and answer "is X a descendant of Y" for drag-drop cycle guards. Kept free of React so
// they can be unit-tested directly.

import type { NodeGroup, NodeSummary } from '../types/api';

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

const byName = <T extends { name: string }>(a: T, b: T) =>
  a.name.localeCompare(b.name);

/** Build the nested tree from the flat group + node lists. Nodes whose `group_id` is null or
 *  points at an unknown group fall into `ungrouped`; groups whose `parent_id` is unknown are
 *  treated as top-level (so a stale reference can never hide a row). Children and nodes are
 *  name-sorted for stable display. */
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
    g.children.sort(byName);
    g.nodes.sort(byName);
  }
  roots.sort(byName);
  ungrouped.sort(byName);
  return { roots, ungrouped };
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
