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

import { flatRowKey, isSelfOrDescendant, rowDepth, type FlatRow } from '../../lib/nodeTree';
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
 * - Over a **group**, the top and bottom quarters are before/after and the middle half is `inside`.
 *   The middle is the widest band because nesting is the common intent and the one with no
 *   keyboard alternative.
 * - Over a **node**, the row splits in half: there is no `inside` a node.
 *
 * 🚨 **The number of nodes being dragged does not change the answer** (ADR-124 増分 8). It used to:
 * a batch over a node row read `inside`, meaning "into that row's folder, appended", because
 * placing N nodes between two rows needed a bulk placement endpoint and there was none — so the
 * same gesture answered differently at one node and at three. `POST /nodes/move` now carries
 * `before`/`after` and writes the whole batch in one statement, so this reads the cursor and
 * nothing else.
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
  if (drag?.kind === 'node' && targetIsGroup) return 'inside';
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
 *
 * 🚨 **A folder may land beside a node** (ADR-162). This branch used to read
 * `target.kind === 'node' ⇒ false`, on the ground that "a folder relates to folders only" — which
 * was true while the renderer drew every folder above every node, and which made the whole area a
 * folder's members occupy undroppable. Under one parent the two kinds are one ordered list, so a
 * node row is a position like any other and the cycle question is the same one: the drop re-parents
 * the folder into **that row's scope**.
 *
 * ⚠️ **Except at the top level, where it is refused** (ADR-162 decision 6). A node with no folder is
 * drawn under the "Ungrouped" header, apart from the top-level folders — the one place the tree is
 * still two lists. The server would happily compute a position there, and the folder would then
 * appear somewhere the operator did not drop it. `scope == null` is exactly that row:
 * `buildNodeTree` sends every node whose `group_id` is null to the ungrouped bucket.
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
  // Dragging a folder: never onto itself.
  if (target.id === drag.id) return false;
  if (target.kind === 'node') {
    // Beside a node: allowed inside a folder, refused under the Ungrouped header (see above).
    // `dropPosition` never answers `inside` over a node row, so this is a before/after landing in
    // that node's own folder — which must not be the folder being dragged, or one beneath it.
    return target.scope != null && !isSelfOrDescendant(groups, drag.id, target.scope);
  }
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
  /** Assign nodes into a group (`null` = top level). One node is a list of one: since Inc.4 every
   *  node move — drag, menu, dialog — goes through `POST /nodes/move`, so there is one answer to
   *  what moving something does.
   *
   *  `before`/`after` name the sibling node to land next to, at most one; neither means append.
   *  🚨 **Since 増分 8 a batch carries them too** — there used to be a separate `reorder-node`
   *  shape that could only hold one id, so a multi-node drop had to fall back to appending. */
  | {
      kind: 'move-nodes';
      nodeIds: readonly string[];
      groupId: string | null;
      before?: string;
      after?: string;
    }
  /** Re-parent a group, appending it. */
  | { kind: 'move-group'; groupId: string; parentId: string | null }
  /** Place a group next to a sibling row under `parentId`.
   *
   *  ⚠️ **`before`/`after` may name a NODE** (ADR-162). Under one parent folders and nodes are one
   *  ordered list, so the anchor is whatever row the cursor was on. The server looks the id up in
   *  the merged sibling list (`groups::ordered_tree_siblings`), which is the only reason this
   *  needed no new field. */
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
    // Beside a node row: into the folder that row sits in, at that row's edge. One id or thirty —
    // `dropPosition` never answers `inside` over a node, so there is no third case here.
    return {
      kind: 'move-nodes',
      nodeIds: drag.ids,
      groupId: target.scope,
      ...(position === 'before' ? { before: target.id } : { after: target.id }),
    };
  }
  if (position === 'inside') return { kind: 'move-group', groupId: drag.id, parentId: target.id };
  // Beside a row, folder or node: into the folder that row sits in, at that row's edge (ADR-162).
  // `dropPosition` never answers `inside` over a node, and the `inside` case above has already
  // taken the folder rows, so both kinds arrive here meaning the same thing.
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

/**
 * What the tree is currently showing about a drag in flight: the row under the cursor, where in that
 * row, and whether the drop is permitted.
 *
 * ⚠️ **It carries the whole [`Target`], not the row's id** (ADR-162 増分 2). The slot row below
 * swallows the pointer where it sits, so the drop that lands there has no row of its own to read a
 * target off — it replays the one recorded here. An id alone could not: `dropAction` needs the
 * target's **scope** to name the folder the drop writes into.
 *
 * `'root'` is the Ungrouped header, which is a drop zone and not a row of the tree.
 */
export type DropFeedback = {
  target: Target | 'root';
  position: DropPos;
  ok: boolean;
};

/**
 * The rows to draw while a drag is in flight: the same list, with **one** slot row inserted exactly
 * where the drop would write.
 *
 * 🚨 **This replaced a 2px insertion line, and the line was not merely ugly — it was ambiguous**
 * (ADR-162 増分 2). A line under a folder's last node and a line over the next folder's row are one
 * pixel row apart and mean different parents: the first lands *inside* the folder above, the second
 * lands beside it. The line could not say which, because it drew the same mark at the same place.
 * A row can: it is drawn at the destination's **depth**, and the 16px of indentation is the answer.
 *
 * Three things it deliberately does not do:
 *
 * - **It does not move the dragged rows.** They stay where they are, dimmed. Lifting a folder's
 *   whole subtree out would shift the rows below it by as many rows as the subtree is deep, which
 *   changes what is under the cursor — and re-judging from there puts the slot somewhere else,
 *   which shifts the rows again. One inserted row moves nothing by more than one row height.
 * - **It answers nothing for `inside`.** That position appends to the end of the target folder's
 *   contents, which is routinely off screen; a slot the operator cannot see is worse than the
 *   outline the row already draws where they *are* looking.
 * - **It returns `flat` itself** — the same reference — whenever there is nothing to show, so a tree
 *   that is not being dragged over renders from exactly the array `flattenTree` produced.
 *
 * ⚠️ `after` steps over the target's whole subtree, not just the target. Dropping after an open
 * folder makes the dragged item that folder's next *sibling*, so the slot belongs below the last
 * row the folder contains; putting it directly under the folder's own row would draw "inside it".
 */
export function withDropSlot(
  flat: readonly FlatRow[],
  drop: DropFeedback | null,
): readonly FlatRow[] {
  if (!drop || !drop.ok || drop.target === 'root' || drop.position === 'inside') return flat;
  // `flatRowKey` is already the one answer to "which row is this" — asking it here is what keeps a
  // loading placeholder under a folder from matching the folder's own id, and what makes a node
  // found whether it is drawn inside a folder or under Ungrouped.
  const want = `${drop.target.kind === 'group' ? 'g' : 'n'}:${drop.target.id}`;
  const at = flat.findIndex((row) => flatRowKey(row) === want);
  if (at < 0) return flat;
  const depth = rowDepth(flat[at]);
  if (depth < 0) return flat;
  let index = at;
  if (drop.position === 'after') {
    index += 1;
    while (index < flat.length && rowDepth(flat[index]) > depth) index += 1;
  }
  return [...flat.slice(0, index), { kind: 'drop-slot', depth }, ...flat.slice(index)];
}

/**
 * The folder the drop would write into, so its row can say so as well.
 *
 * The slot's indentation already implies it; this names it. The two are one gesture apart in the
 * screenshot that motivated the increment — a folder dropped at the bottom edge of `DNS`'s last
 * node goes into `DNS`, and five pixels lower it goes into `DNS`'s own parent.
 *
 * ⚠️ **`null` is not "no destination"** — it is "no destination *row*". A drop at the top level or
 * among the Ungrouped nodes has a real destination and no folder row standing for it, so the slot's
 * indentation is the only mark there. `inside` answers null because the target row is already
 * outlined; marking it twice would say two different things about the same row.
 */
export function dropParentId(drop: DropFeedback | null): string | null {
  if (!drop || !drop.ok || drop.target === 'root' || drop.position === 'inside') return null;
  return drop.target.scope;
}

/** What the insertion slot shows: the row the pointer grabbed, so the operator can see *what* is
 *  about to land as well as where. `extra` is how many further nodes travel with it — a batch drag
 *  carries the working set, and a slot naming one row of thirty would understate the move. */
export type DragPreview =
  | { kind: 'group'; group: NodeGroup }
  | { kind: 'node'; name: string; extra: number };

/**
 * Resolve what a drag is carrying, for the slot to draw.
 *
 * ⚠️ **Looked up rather than captured at `dragstart`.** [`DragItem`] is what a drop *writes*, and
 * every branch of `dropAction` is built from it; adding a display string to it would put a second
 * kind of fact in the payload and invite the next reader to place a node by its name. The folder
 * comes from the group list the tree already holds, and the node from the rows already on screen.
 *
 * `null` when the grabbed row is not among the rows in hand — a node whose folder collapsed
 * mid-drag, say. The slot is still drawn: **where** the drop lands is the answer it exists to give,
 * and it does not stop being true because the name is momentarily unavailable.
 */
export function dragPreview(
  flat: readonly FlatRow[],
  groups: readonly NodeGroup[],
  drag: DragItem | null,
): DragPreview | null {
  if (!drag) return null;
  if (drag.kind === 'group') {
    const group = groups.find((g) => g.id === drag.id);
    return group ? { kind: 'group', group } : null;
  }
  const row = flat.find(
    (r) => (r.kind === 'node' || r.kind === 'ungrouped-node') && r.node.id === drag.id,
  );
  if (!row || !('node' in row)) return null;
  return { kind: 'node', name: row.node.name, extra: drag.ids.length - 1 };
}
