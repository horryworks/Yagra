// SPDX-License-Identifier: AGPL-3.0-only
// What a drag carries, where it lands, and whether it may.
//
// This is the tree's most consequential judgement and it had no test of any kind: the branches below
// choose which callback fires and with which arguments, and getting one wrong moves a node into the
// wrong group — a write that succeeds, looks deliberate, and is only visible to whoever notices
// their device is filed somewhere else. It sat in `NodeTree.tsx`, and Vitest never loads a `.tsx`
// (`environment: 'node'` + `include: ['src/**/*.test.ts']`, see testing.md).
//
// Nothing here touches the DOM or React. [`dropPosition`] takes a Y offset and a height rather than
// a `DragEvent`, which is the same shape the resize handles use (`ui-conventions.md`: "the
// arithmetic lives in a `.ts` beside the component") and for the same reason — the arithmetic is
// the half that can be silently wrong.
//
// 🚨 **A drag carries what the row's own move affordance carries** (ADR-124 Inc.4). Until then the
// payload was one id, `onDragStart` never read the working set, and dragging three checked nodes
// moved exactly the one the pointer had grabbed. That is the defect Inc.2 fixed for the right-click
// menu and the hover ↗ — it survived here because this path answered the row-vs-selection question
// by not asking it. The question is `actsOnSelection`, and this module asks it in exactly one place
// ([`nodeDragItem`]); nothing else here decides what is in a batch.

import { isSelfOrDescendant } from '../../lib/nodeTree';
import type { NodeGroup } from '../../types/api';
import { actsOnSelection } from './nodeTreeSelect';

/**
 * What is being dragged.
 *
 * ⚠️ **Only the node side carries a set.** The working set holds nodes and nothing else, so a folder
 * drag is always exactly one folder — and giving `group` an `ids` would make
 * `{ kind: 'group', id: 'a', ids: ['a', 'b'] }` a value someone can construct and nothing can mean.
 */
export type DragItem =
  /** `id` is the row the pointer grabbed — it drives the drop indicator and the reorder branch.
   *  `ids` is every node that will move, in the order they will be written, and **always contains
   *  `id`**. One node is a list of one, the same way `MoveNodeModal` takes a list of one. */
  | { kind: 'node'; id: string; ids: readonly string[] }
  | { kind: 'group'; id: string };

/** The node side of [`DragItem`]. Named because [`nodeDragItem`] always returns this one, and a
 *  caller reading `.ids` off the union has to narrow a case that cannot happen. */
export type NodeDrag = Extract<DragItem, { kind: 'node' }>;

/**
 * What a drag starting on a node row carries.
 *
 * 🚨 **This is the whole of the Inc.4 fix, and it is here so a test can run it.** The `.tsx` used to
 * write `{ kind: 'node', id: node.id }` inline, eight lines below its own call to `nodeMoveItems` —
 * the answer was already on screen and the drag did not read it.
 *
 * The order of `ids` is the working set's **insertion order**, which is what `MoveNodeModal` and the
 * selection bar already send. It is not cosmetic: `POST /nodes/move` assigns `sort_order` from the
 * array position, so the order reaches the destination folder. Two paths sending the same nodes in
 * different orders would be two answers to "what happens when you move something".
 */
export function nodeDragItem(checked: ReadonlyMap<string, unknown>, nodeId: string): NodeDrag {
  return {
    kind: 'node',
    id: nodeId,
    ids: actsOnSelection(checked, nodeId) ? [...checked.keys()] : [nodeId],
  };
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
 * - A **batch over a node** is always `inside` too, meaning "into that row's folder, appended" —
 *   see below.
 * - Over a **group**, the top and bottom quarters are before/after and the middle half is `inside`.
 *   The middle is the widest band because nesting is the common intent and the one with no
 *   keyboard alternative.
 * - Over a **node**, the row splits in half: there is no `inside` a node.
 *
 * 🚨 **A batch has no insertion point, deliberately** (ADR-124 Inc.4 決定 C). Placing N nodes
 * between two rows would need a bulk placement endpoint, and there is none —
 * `PUT /nodes/{id}/placement` takes one node. Calling it N times is a write that can fail halfway
 * with no way for the operator to read how far it got, which is the failure 決定 7 refuses when it
 * rejects an oversized batch instead of truncating it. So the batch appends, and the indicator says
 * so: a row outline rather than an insertion line. ⚠️ The cost is that the same gesture answers
 * differently at one node and at three; it is on the backlog with its unblocking condition.
 *
 * ⚠️ `height` of 0 is treated as 1. A row measured mid-layout reports 0, and dividing by it would
 * make every comparison `NaN` — which compares false, so every drop would silently read `after`.
 */
export function dropPosition(
  offsetY: number,
  height: number,
  targetIsGroup: boolean,
  drag: DragItem | null,
): DropPos {
  if (drag?.kind === 'node' && (targetIsGroup || drag.ids.length > 1)) return 'inside';
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
 *
 * ⚠️ **The node branch refuses the whole batch, not just the grabbed row** (Inc.4). "Not onto
 * itself" generalises to "not onto anything I am carrying" — dropping three nodes onto the second
 * of the three names a destination that is one of the things being moved.
 */
export function dropAllowed(
  groups: NodeGroup[],
  drag: DragItem | null,
  target: Target,
  position: DropPos,
): boolean {
  if (!drag) return false;
  if (drag.kind === 'node') {
    // Nodes can reorder next to another node or be assigned into a group, but never onto one of
    // the rows being dragged.
    return !(target.kind === 'node' && drag.ids.includes(target.id));
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
  /** Assign nodes into a group (`null` = top level), appending them. One node is a list of one:
   *  since Inc.4 every node move — drag, menu, dialog — goes through `POST /nodes/move`, so there
   *  is one answer to what moving something does. */
  | { kind: 'move-nodes'; nodeIds: readonly string[]; groupId: string | null }
  /** Re-parent a group, appending it. */
  | { kind: 'move-group'; groupId: string; parentId: string | null }
  /** Place **one** node next to a sibling node inside `groupId`. ⚠️ Unreachable for a batch by
   *  construction — `dropPosition` returns `inside` above one — because there is no bulk placement
   *  endpoint to carry it. */
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
 * The batch branch reads that same scope for the same reason.
 */
export function dropAction(drag: DragItem, target: Target, position: DropPos): DropAction {
  if (drag.kind === 'node') {
    if (target.kind === 'group') {
      return { kind: 'move-nodes', nodeIds: drag.ids, groupId: target.id };
    }
    // A batch over a node row: into the folder that row sits in, appended (see `dropPosition`).
    if (position === 'inside') {
      return { kind: 'move-nodes', nodeIds: drag.ids, groupId: target.scope };
    }
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
    ? { kind: 'move-nodes', nodeIds: drag.ids, groupId: null }
    : { kind: 'move-group', groupId: drag.id, parentId: null };
}
