// SPDX-License-Identifier: AGPL-3.0-only
// What "Pinned only" keeps in the inventory tree (ADR-146).
//
// A pin is one account saying "I look at this often". Pinned only shows every pinned folder with
// everything beneath it, every pinned node, and the folders above both — as a path, without their
// own members. The decision lives here rather than in `NodeTree.tsx`, because Vitest never runs a
// `.tsx` (`tsxJudgement.test.ts`).

import type { NodeGroup, NodeSummary } from '../types/api';

/** Which folders and nodes Pinned only keeps. */
export interface PinnedView {
  /** Pinned folders — the ones that carry the pin mark. */
  groups: ReadonlySet<string>;
  /** Pinned nodes. */
  nodes: ReadonlySet<string>;
  /** Pinned folders and every folder beneath them: shown whole, members and all. */
  subtree: ReadonlySet<string>;
  /** Folders above something pinned that are not themselves inside a pinned folder: shown as a
   *  path, with only the pinned things beneath them. */
  ancestors: ReadonlySet<string>;
}

/**
 * Work out what Pinned only keeps.
 *
 * ⚠️ **A pinned node's folder comes from `pinnedNodes`, the server's rows** — not from the members
 * the tree happens to have loaded. That is the whole reason the pins endpoint returns full rows: a
 * pinned node usually sits in a folder nobody has opened, and without its `group_id` there is no
 * path to draw.
 *
 * A pinned folder missing from `groups` (deleted since, or outside the caller's scope) draws
 * nothing. The walk up is bounded by the folder count, like `groupTrail`, so cyclic data cannot loop.
 */
export function pinnedView(
  groups: readonly NodeGroup[],
  pinnedGroupIds: ReadonlySet<string>,
  pinnedNodeIds: ReadonlySet<string>,
  pinnedNodes: readonly Pick<NodeSummary, 'id' | 'group_id'>[],
): PinnedView {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const childrenOf = new Map<string, string[]>();
  for (const g of groups) {
    if (!g.parent_id) continue;
    const list = childrenOf.get(g.parent_id) ?? [];
    list.push(g.id);
    childrenOf.set(g.parent_id, list);
  }

  const subtree = new Set<string>();
  const walkDown = (id: string) => {
    if (subtree.has(id)) return;
    subtree.add(id);
    for (const child of childrenOf.get(id) ?? []) walkDown(child);
  };
  for (const id of pinnedGroupIds) if (byId.has(id)) walkDown(id);

  const ancestors = new Set<string>();
  const walkUp = (start: string | null | undefined) => {
    let id = start;
    for (let i = 0; id && i <= groups.length; i++) {
      const g = byId.get(id);
      if (!g) return;
      if (!subtree.has(id)) ancestors.add(id);
      id = g.parent_id;
    }
  };
  for (const id of pinnedGroupIds) {
    const g = byId.get(id);
    if (g) walkUp(g.parent_id);
  }
  // The node's own folder is walked too: it is the last step of the node's path.
  for (const n of pinnedNodes) if (pinnedNodeIds.has(n.id)) walkUp(n.group_id);

  return { groups: pinnedGroupIds, nodes: pinnedNodeIds, subtree, ancestors };
}

/** Whether Pinned only keeps this folder's row. */
export function pinnedGroupShown(view: PinnedView, groupId: string): boolean {
  return view.subtree.has(groupId) || view.ancestors.has(groupId);
}

/** Whether Pinned only keeps this node's row: it is pinned itself, or it sits somewhere inside a
 *  pinned folder. */
export function pinnedNodeShown(
  view: PinnedView,
  node: Pick<NodeSummary, 'id' | 'group_id'>,
): boolean {
  return view.nodes.has(node.id) || (node.group_id != null && view.subtree.has(node.group_id));
}

/** Nothing is pinned — Pinned only then says how to pin rather than drawing an empty tree
 *  (ADR-055 R6: say it where the operator is looking). */
export function nothingPinned(view: PinnedView): boolean {
  return view.groups.size === 0 && view.nodes.size === 0;
}

/** `set` with `id` in it (`on`) or out of it. A copy, so a store holding the result sees a change. */
export function withMember(set: ReadonlySet<string>, id: string, on: boolean): Set<string> {
  const next = new Set(set);
  if (on) next.add(id);
  else next.delete(id);
  return next;
}
