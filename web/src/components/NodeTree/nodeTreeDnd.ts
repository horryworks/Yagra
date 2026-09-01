// SPDX-License-Identifier: AGPL-3.0-only
// Where a dragged row lands, and whether it may.
//
// This is the tree's most consequential judgement and it had no test of any kind: the four branches
// below choose which of four callbacks fires and with which arguments, and getting one wrong moves
// a node into the wrong group — a write that succeeds, looks deliberate, and is only visible to
// whoever notices their device is filed somewhere else. It sat in `NodeTree.tsx`, and Vitest never
// loads a `.tsx` (`environment: 'node'` + `include: ['src/**/*.test.ts']`, see testing.md).
//
// Nothing here touches the DOM or React. [`dropPosition`] takes a Y offset and a height rather than
// a `DragEvent`, which is the same shape the resize handles use (`ui-conventions.md`: "the
// arithmetic lives in a `.ts` beside the component") and for the same reason — the arithmetic is
// the half that can be silently wrong.

import { isSelfOrDescendant } from '../../lib/nodeTree';
import type { NodeGroup } from '../../types/api';

/** What is being dragged. */
export interface DragItem {
  kind: 'node' | 'group';
  id: string;
}

/** Where the cursor is in the target row. */
export type DropPos = 'before' | 'after' | 'inside';

/** What the drop landed on: a group or node row, with the sibling scope it sits in
 *  (`scope` is the group id that owns the target — `null` at the top level). */
export type Target =
  | { kind: 'group'; id: string; scope: string | null }
  | { kind: 'node'; id: string; scope: string | null };

/**
 * Read the drop position from the cursor's vertical position within the target row.
 *
 * - A **node over a group** is always `inside`: a node cannot be a sibling of a group, so the
 *   before/after bands would offer a placement that has no meaning.
 * - Over a **group**, the top and bottom quarters are before/after and the middle half is `inside`.
 *   The middle is the widest band because nesting is the common intent and the one with no
 *   keyboard alternative.
 * - Over a **node**, the row splits in half: there is no `inside` a node.
 *
 * ⚠️ `height` of 0 is treated as 1. A row measured mid-layout reports 0, and dividing by it would
 * make every comparison `NaN` — which compares false, so every drop would silently read `after`.
 */
export function dropPosition(
  offsetY: number,
  height: number,
  targetIsGroup: boolean,
  dragKind: 'node' | 'group' | null,
): DropPos {
  if (dragKind === 'node' && targetIsGroup) return 'inside';
  const h = height || 1;
  if (targetIsGroup) {
    if (offsetY < h * 0.25) return 'before';
    if (offsetY > h * 0.75) return 'after';
    return 'inside';
  }
  return offsetY < h * 0.5 ? 'before' : 'after';
}

/**
 * Whether this drag may drop here.
 *
 * The cycle guard is the point: nesting a group inside its own subtree would orphan every
 * descendant, and re-parenting it *beside* a descendant does the same thing one level up — which
 * is why the `before`/`after` case checks the target's **scope**, not the target.
 */
export function dropAllowed(
  groups: NodeGroup[],
  drag: DragItem | null,
  target: Target,
  position: DropPos,
): boolean {
  if (!drag) return false;
  if (drag.kind === 'node') {
    // A node can reorder next to another node or be assigned into a group, but not onto itself.
    return !(target.kind === 'node' && target.id === drag.id);
  }
  // Dragging a group: it relates to groups only, never to a node, and never to itself.
  if (target.kind === 'node' || target.id === drag.id) return false;
  if (position === 'inside') return !isSelfOrDescendant(groups, drag.id, target.id);
  // before/after re-parents the group to the target's parent scope.
  return target.scope == null || !isSelfOrDescendant(groups, drag.id, target.scope);
}

/**
 * What a permitted drop should do, as a value.
 *
 * Four shapes, one per callback the tree exposes. Returning a value rather than calling straight
 * through is what makes this testable at all — the component switches on it once, and every
 * argument that reaches a write is decided here.
 */
export type DropAction =
  /** Assign a node into a group, appending it. */
  | { kind: 'move-node'; nodeId: string; groupId: string | null }
  /** Re-parent a group, appending it. */
  | { kind: 'move-group'; groupId: string; parentId: string | null }
  /** Place a node next to a sibling node inside `groupId`. */
  | { kind: 'reorder-node'; nodeId: string; groupId: string | null; before?: string; after?: string }
  /** Place a group next to a sibling group under `parentId`. */
  | {
      kind: 'reorder-group';
      groupId: string;
      parentId: string | null;
      before?: string;
      after?: string;
    };

/**
 * The action a drop on a row performs.
 *
 * ⚠️ Note which id each branch carries. A reorder names the **target's** scope, not the dragged
 * item's — dropping a node beside a node in another group both moves and orders it, and reading
 * the dragged node's own group here would leave it where it was while claiming to have moved it.
 */
export function dropAction(drag: DragItem, target: Target, position: DropPos): DropAction {
  if (drag.kind === 'node') {
    if (target.kind === 'group') return { kind: 'move-node', nodeId: drag.id, groupId: target.id };
    return {
      kind: 'reorder-node',
      nodeId: drag.id,
      groupId: target.scope,
      ...(position === 'before' ? { before: target.id } : { after: target.id }),
    };
  }
  if (position === 'inside') return { kind: 'move-group', groupId: drag.id, parentId: target.id };
  return {
    kind: 'reorder-group',
    groupId: drag.id,
    parentId: target.scope,
    ...(position === 'before' ? { before: target.id } : { after: target.id }),
  };
}

/** What a drop on the "Ungrouped" header does: move to the top level, whichever kind is dragged. */
export function rootDropAction(drag: DragItem): DropAction {
  return drag.kind === 'node'
    ? { kind: 'move-node', nodeId: drag.id, groupId: null }
    : { kind: 'move-group', groupId: drag.id, parentId: null };
}
