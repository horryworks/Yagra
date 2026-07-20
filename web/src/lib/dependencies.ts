// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers for the dependency graph (single-parent forest). Used by the "set upstream"
// picker to exclude choices that would form a cycle, and by the dependency page. The server
// still validates every edge (self/cycle) — these keep the UI from offering an invalid choice.

import type { TopologyNode } from '../types/api';

/** Every node downstream of `rootId` (its transitive children) in the dependency forest, keyed by
 *  each node's `parent_id`. Excludes `rootId` itself. Cycle-safe: each node is visited at most
 *  once, so malformed (already-cyclic) data can't loop forever. */
export function descendantIds(nodes: TopologyNode[], rootId: string): Set<string> {
  const children = new Map<string, string[]>();
  for (const n of nodes) {
    if (n.parent_id) {
      const arr = children.get(n.parent_id);
      if (arr) arr.push(n.id);
      else children.set(n.parent_id, [n.id]);
    }
  }
  const out = new Set<string>();
  const queue = [rootId];
  while (queue.length) {
    const cur = queue.shift() as string;
    for (const child of children.get(cur) ?? []) {
      if (child !== rootId && !out.has(child)) {
        out.add(child);
        queue.push(child);
      }
    }
  }
  return out;
}

/** The set of nodes that must NOT be offered as `nodeId`'s upstream parent: the node itself plus
 *  every descendant (choosing either would create a dependency cycle). */
export function invalidParentIds(nodes: TopologyNode[], nodeId: string): Set<string> {
  const set = descendantIds(nodes, nodeId);
  set.add(nodeId);
  return set;
}
