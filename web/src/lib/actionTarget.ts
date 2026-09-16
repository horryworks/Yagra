// SPDX-License-Identifier: AGPL-3.0-only
// What an action started from the inventory tree acts on: one row, or the whole working set.
//
// 🚨 **Every batch-aware action needs this, and before ADR-124 増分 10 each one invented it.** The
// moves took `readonly string[]`, the dialogs took `NodeSummary[]`, and the pool and suppression
// handlers took a single `SuppressionTarget` — which is exactly why they kept acting on one row
// while sitting in a menu headed "Move 20 selected…". Giving them all one argument type is what
// lets `nodeActionItems` decide the scope in one place and the handlers simply carry it.
//
// The folder case is not a set: a folder-wide pool or maintenance window is a different write
// (`PUT /node-groups/{id}/pool`), reaching every node beneath it by inheritance rather than by id.
//
// Pure, so the arithmetic below is unit-tested (`.tsx` is never loaded by Vitest — testing.md).

import type { NodeSummary } from '../types/api';
import type { SuppressionTarget } from './suppression';

/** Several nodes, in the order the operator checked them. */
export interface NodesTarget {
  kind: 'nodes';
  nodes: readonly NodeSummary[];
}

/** One row (a node or a folder), or a set of nodes. */
export type ActionTarget = SuppressionTarget | NodesTarget;

/** The node ids an action on this target writes to. A folder has none of its own — it is written
 *  through its own endpoint, not by listing its members — so this is empty for a group. */
export function targetNodeIds(target: ActionTarget): string[] {
  if (target.kind === 'nodes') return target.nodes.map((n) => n.id);
  return target.kind === 'node' ? [target.id] : [];
}

/** How many nodes the action names. Used for the "N selected" labels and the dialog headings. */
export function targetNodeCount(target: ActionTarget): number {
  return target.kind === 'nodes' ? target.nodes.length : target.kind === 'node' ? 1 : 0;
}

/** The names to list in a dialog that is about to write to several nodes. Empty for a single row
 *  or a folder, both of which name themselves in the dialog's title instead. */
export function targetNodeNames(target: ActionTarget): string[] {
  return target.kind === 'nodes' ? target.nodes.map((n) => n.name) : [];
}
