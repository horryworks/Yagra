// SPDX-License-Identifier: AGPL-3.0-only
// The tree's drop judgement — the branches choosing which write fires and with which arguments, and
// until ADR-052 Inc.6 they were in `NodeTree.tsx`, which Vitest never loads.
//
// What makes this worth testing is the failure shape: a wrong branch does not throw and does not
// look broken. It moves a node into a different group, the write succeeds, and the only person who
// finds out is whoever later wonders why their device is filed somewhere else.
//
// 🚨 **Every case here took a single-id payload until ADR-124 Inc.4, and that is why the suite was
// green while a drag of three checked nodes moved one.** The tests were complete about the branch
// and silent about the cargo — `dropAction` was asked what to do with `{ id: 'n1' }` and answered
// correctly, forever, because nothing ever handed it a batch. Read that as the shape to watch for:
// a test file that exercises every branch of a decision can still say nothing about the value the
// decision is made on.
import { describe, expect, it } from 'vitest';
import {
  type DragItem,
  dragPreview,
  dropAction,
  dropAllowed,
  type DropFeedback,
  dropParentId,
  type DropPos,
  dropPosition,
  dropToPerform,
  nodeDragItem,
  rootDropAction,
  type Target,
  withDropSlot,
} from './nodeTreeDnd';
import type { FlatRow, TreeGroup } from '../../lib/nodeTree';
import type { NodeGroup, NodeSummary } from '../../types/api';

const grp = (id: string, parent_id: string | null = null) =>
  ({ id, name: id, parent_id }) as NodeGroup;

/** site → rack → shelf, plus an unrelated top-level group. */
const GROUPS = [grp('site'), grp('rack', 'site'), grp('shelf', 'rack'), grp('other')];

const groupTarget = (id: string, scope: string | null = null): Target => ({
  kind: 'group',
  id,
  scope,
});
const nodeTarget = (id: string, scope: string | null = null): Target => ({ kind: 'node', id, scope });

/** A node drag. The first id is the row the pointer grabbed; all of them move. */
const nodeDrag = (...ids: string[]): DragItem => ({ kind: 'node', id: ids[0], ids });
const groupDrag = (id: string): DragItem => ({ kind: 'group', id });
/** A node drag where the grabbed row is NOT the first of the batch — the ordinary case, since
 *  the operator checks three rows and then grabs whichever one the pointer is over. */
const grabbed = (id: string, ...ids: string[]): DragItem => ({ kind: 'node', id, ids });

/** A working set, in the order the rows were checked — which is the order they will be written. */
const checked = (...ids: string[]) => new Map(ids.map((id) => [id, {}]));

describe('nodeDragItem', () => {
  it('carries the whole working set when the grabbed row is in it', () => {
    // 🚨 THE REGRESSION (Inc.4). The operator checks three rows, grabs one of them, and the drag
    // used to carry that one — so the tree painted three rows and moved one.
    const item = nodeDragItem(checked('a', 'b', 'c'), 'b');
    expect(item).toEqual({ kind: 'node', id: 'b', ids: ['a', 'b', 'c'] });
  });

  it('keeps the working set’s own order, because the order reaches the folder', () => {
    // `POST /nodes/move` assigns sort_order from the array position, so this is not cosmetic —
    // and it is the order `MoveNodeModal` and the selection bar already send.
    expect(nodeDragItem(checked('c', 'a', 'b'), 'a').ids).toEqual(['c', 'a', 'b']);
  });

  it('carries only the grabbed row when it is outside the working set', () => {
    // A row outside the batch is its own gesture — the same answer the right-click menu gives it.
    // The batch is left alone; the page is what decides not to clear it.
    expect(nodeDragItem(checked('a', 'b'), 'z').ids).toEqual(['z']);
  });

  it('carries only the grabbed row when the working set is empty, or is just that row', () => {
    expect(nodeDragItem(new Map(), 'a').ids).toEqual(['a']);
    // A set of one is the row: there is nothing else in it to carry.
    expect(nodeDragItem(checked('a'), 'a').ids).toEqual(['a']);
  });
});

describe('dropPosition', () => {
  it('always nests a node dropped on a group', () => {
    // A node cannot be a sibling of a group, so the before/after bands would offer a placement
    // that has no meaning.
    for (const y of [0, 5, 15, 29]) expect(dropPosition(y, 30, true, nodeDrag('n1'))).toBe('inside');
  });

  it('splits a node row the same way however many nodes are being dragged', () => {
    // 🚨 THE 増分 8 REGRESSION. This used to answer `inside` for every Y once the batch held more
    // than one node — an append — because there was no bulk placement endpoint to carry an
    // insertion point. `POST /nodes/move` takes `before`/`after` now, so the count is not part of
    // the question: the same gesture must answer the same way at one node and at three.
    const batch = nodeDrag('n1', 'n2', 'n3');
    expect(dropPosition(0, 30, false, batch)).toBe('before');
    expect(dropPosition(14, 30, false, batch)).toBe('before');
    expect(dropPosition(15, 30, false, batch)).toBe('after');
    expect(dropPosition(29, 30, false, batch)).toBe('after');
    // And a batch over a *group* is still always `inside` — a node is never a group's sibling.
    for (const y of [0, 5, 15, 29]) expect(dropPosition(y, 30, true, batch)).toBe('inside');
  });

  it('splits a group row into quarter / half / quarter', () => {
    const g = groupDrag('g1');
    expect(dropPosition(0, 40, true, g)).toBe('before');
    expect(dropPosition(9, 40, true, g)).toBe('before');
    expect(dropPosition(10, 40, true, g)).toBe('inside'); // exactly 25% is already inside
    expect(dropPosition(20, 40, true, g)).toBe('inside');
    expect(dropPosition(30, 40, true, g)).toBe('inside'); // exactly 75% is still inside
    expect(dropPosition(31, 40, true, g)).toBe('after');
  });

  it('splits a node row in half for ONE node — there is no inside a node', () => {
    expect(dropPosition(0, 30, false, nodeDrag('n1'))).toBe('before');
    expect(dropPosition(14, 30, false, nodeDrag('n1'))).toBe('before');
    expect(dropPosition(15, 30, false, nodeDrag('n1'))).toBe('after'); // exactly half is after
    expect(dropPosition(29, 30, false, groupDrag('g1'))).toBe('after');
  });

  it('does not divide by a zero height', () => {
    // 🚨 A row measured mid-layout reports 0. Dividing by it makes every comparison NaN, which is
    // false — so every drop would silently read `after`, on both kinds of row.
    expect(dropPosition(0, 0, true, groupDrag('g1'))).toBe('before');
    expect(dropPosition(0, 0, false, groupDrag('g1'))).toBe('before');
  });

  it('treats an absent drag like a group drag rather than short-circuiting', () => {
    // `drag` is null between the pointer entering a row and the drag starting; the row still has
    // to compute a position for its hover feedback.
    expect(dropPosition(20, 40, true, null)).toBe('inside');
  });
});

describe('dropAllowed', () => {
  it('refuses everything while nothing is being dragged', () => {
    expect(dropAllowed(GROUPS, null, groupTarget('site'), 'inside')).toBe(false);
  });

  it('lets a node go into any group and beside any other node', () => {
    const n = nodeDrag('n1');
    expect(dropAllowed(GROUPS, n, groupTarget('site'), 'inside')).toBe(true);
    expect(dropAllowed(GROUPS, n, nodeTarget('n2', 'site'), 'before')).toBe(true);
  });

  it('refuses a node dropped on itself', () => {
    expect(dropAllowed(GROUPS, nodeDrag('n1'), nodeTarget('n1', 'site'), 'before')).toBe(false);
  });

  it('refuses a batch dropped on ANY row it is carrying', () => {
    // ⚠️ "Not onto itself" generalises to "not onto anything I am carrying" (Inc.4): the second of
    // three would otherwise name a destination that is one of the things being moved. Checking
    // `drag.id` alone — the grabbed row — permits exactly that.
    //
    // ⚠️ The positions here are the ones a node row can actually produce. They were `inside` until
    // 増分 8, which was the only answer a batch over a node row used to get — a refusal asserted
    // against a position the cursor can no longer report proves nothing.
    const batch = nodeDrag('n1', 'n2', 'n3');
    expect(dropAllowed(GROUPS, batch, nodeTarget('n2', 'site'), 'before')).toBe(false);
    expect(dropAllowed(GROUPS, batch, nodeTarget('n3', 'site'), 'after')).toBe(false);
    // A row outside the batch is a destination like any other.
    expect(dropAllowed(GROUPS, batch, nodeTarget('n9', 'site'), 'before')).toBe(true);
    expect(dropAllowed(GROUPS, batch, groupTarget('site'), 'inside')).toBe(true);
  });

  it('lets a folder land beside a node inside another folder', () => {
    // 🚨 **The ADR-162 change, and the whole point of it.** This read `toBe(false)` for every node
    // row: "a folder relates to folders only". True while the renderer drew every folder above
    // every node — and it made the entire area a folder's members occupy undroppable, which is
    // what the operator hit. The drop re-parents `site` into `other`, at that node's edge.
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', 'other'), 'before')).toBe(true);
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', 'other'), 'after')).toBe(true);
  });

  it('refuses a folder dropped among the Ungrouped nodes', () => {
    // ADR-162 decision 6: top-level nodes are drawn under their own header, apart from the
    // top-level folders, so a folder dropped there would appear somewhere else. `scope === null`
    // is exactly that row — `buildNodeTree` sends every node with no `group_id` to that bucket.
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', null), 'before')).toBe(false);
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', null), 'after')).toBe(false);
  });

  it('refuses a folder dropped beside a node that sits inside its own subtree', () => {
    // The cycle guard again, reached through the new branch: landing beside a node in `shelf`
    // re-parents `site` into `rack`, which is beneath it.
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', 'shelf'), 'before')).toBe(false);
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', 'site'), 'after')).toBe(false);
  });

  it('refuses a group dropped on itself', () => {
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, groupTarget('site'), 'inside')).toBe(false);
  });

  it('refuses nesting a group inside its own subtree', () => {
    // The cycle guard. Allowing it orphans every descendant.
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, groupTarget('rack', 'site'), 'inside')).toBe(false);
    expect(dropAllowed(GROUPS, g, groupTarget('shelf', 'rack'), 'inside')).toBe(false);
  });

  it('refuses re-parenting a group BESIDE one of its own descendants', () => {
    // 🚨 The subtle half. `before`/`after` re-parents to the target's SCOPE, so dropping `site`
    // beside `shelf` would put `site` inside `rack` — the same cycle, one level up. Checking the
    // target rather than its scope here would let it through.
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, groupTarget('shelf', 'rack'), 'before')).toBe(false);
    expect(dropAllowed(GROUPS, g, groupTarget('rack', 'site'), 'after')).toBe(false);
  });

  it('permits a group moving to the top level or under an unrelated one', () => {
    const g = groupDrag('rack');
    expect(dropAllowed(GROUPS, g, groupTarget('other', null), 'before')).toBe(true);
    expect(dropAllowed(GROUPS, g, groupTarget('other'), 'inside')).toBe(true);
  });
});

describe('dropAction', () => {
  it('assigns a node into the group it was dropped on', () => {
    expect(dropAction(nodeDrag('n1'), groupTarget('site'), 'inside')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1'],
      groupId: 'site',
    });
  });

  it('carries every checked node into the group, not just the grabbed row', () => {
    // 🚨 THE REGRESSION, at the far end of the same gesture. The action is where the arguments to
    // the write are decided, so this is the assertion that would have failed all along.
    expect(dropAction(grabbed('n2', 'n1', 'n2', 'n3'), groupTarget('site'), 'inside')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1', 'n2', 'n3'],
      groupId: 'site',
    });
  });

  it('orders a WHOLE BATCH against a sibling, in the TARGET’s group', () => {
    // 🚨 THE 増分 8 FIX. This branch used to be `reorder-node`, which held a single `nodeId`, so a
    // multi-node drop could not reach it at all — `dropPosition` sent batches to an append
    // instead. Every id must arrive, in the working set's order, with the anchor.
    //
    // 🚨 `groupId` is the target's scope, not the dragged node's. Dropping a node beside a node in
    // another group both moves and orders it; reading the dragged node's own group would leave it
    // where it was while claiming to have moved it.
    expect(dropAction(grabbed('n2', 'n1', 'n2'), nodeTarget('n9', 'rack'), 'before')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1', 'n2'],
      groupId: 'rack',
      before: 'n9',
    });
    expect(dropAction(grabbed('n2', 'n1', 'n2'), nodeTarget('n9', null), 'after')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1', 'n2'],
      groupId: null,
      after: 'n9',
    });
  });

  it('orders ONE node against a sibling through the same shape', () => {
    // One node is a list of one (Inc.4 決定 D), so the drag has one answer to give whatever it is
    // carrying — there is no longer a separate single-node action for the server to serve.
    expect(dropAction(nodeDrag('n1'), nodeTarget('n2', 'rack'), 'before')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1'],
      groupId: 'rack',
      before: 'n2',
    });
    expect(dropAction(nodeDrag('n1'), nodeTarget('n2', 'rack'), 'after')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1'],
      groupId: 'rack',
      after: 'n2',
    });
  });

  it('nests a group when dropped in the middle, and re-parents it when dropped on an edge', () => {
    expect(dropAction(groupDrag('rack'), groupTarget('other'), 'inside')).toEqual({
      kind: 'move-group',
      groupId: 'rack',
      parentId: 'other',
    });
    expect(dropAction(groupDrag('rack'), groupTarget('other', null), 'before')).toEqual({
      kind: 'reorder-group',
      groupId: 'rack',
      parentId: null,
      before: 'other',
    });
  });

  it('anchors a folder against the NODE it was dropped beside', () => {
    // 🚨 The claim ADR-162 rests on, as a value: `before` carries a **node** id and `parentId` is
    // that node's folder. Nothing on the wire changed for this — the server looks the anchor up in
    // the merged sibling list (`groups::ordered_tree_siblings`), which is why `GroupPlacement` did
    // not need a second field and the OpenAPI document did not move.
    expect(dropAction(groupDrag('rack'), nodeTarget('n7', 'other'), 'before')).toEqual({
      kind: 'reorder-group',
      groupId: 'rack',
      parentId: 'other',
      before: 'n7',
    });
    expect(dropAction(groupDrag('rack'), nodeTarget('n7', 'other'), 'after')).toEqual({
      kind: 'reorder-group',
      groupId: 'rack',
      parentId: 'other',
      after: 'n7',
    });
  });

  it('carries exactly one of before/after, never both', () => {
    // The callbacks take `{ before? , after? }` and a value with both would be ambiguous at the
    // server, where the two are separate ordering hints.
    for (const pos of ['before', 'after'] as const) {
      const a = dropAction(nodeDrag('n1'), nodeTarget('n2', null), pos);
      expect('before' in a && 'after' in a).toBe(false);
    }
  });
});

describe('rootDropAction', () => {
  it('moves either kind to the top level', () => {
    expect(rootDropAction(nodeDrag('n1'))).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1'],
      groupId: null,
    });
    expect(rootDropAction(groupDrag('rack'))).toEqual({
      kind: 'move-group',
      groupId: 'rack',
      parentId: null,
    });
  });

  it('ungroups the whole batch, not the grabbed row', () => {
    // The Ungrouped header is the third destination (after a folder row and a node row) and it had
    // its own copy of the single-id assumption.
    expect(rootDropAction(grabbed('n2', 'n1', 'n2'))).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1', 'n2'],
      groupId: null,
    });
  });
});

// ------------------------------------------------------------------------------------------------
// The insertion slot (ADR-162 増分 2)
//
// 🚨 **The first two cases below are the whole increment, and they land at the same index.** A folder
// dropped at the bottom edge of `DNS`'s last node and one dropped at the top edge of the next
// folder's row appear in the same place on screen and go into different parents — which is what the
// operator asked about, and what a 2px line drawn on somebody else's edge could not answer. The only
// thing that distinguishes them is the slot's DEPTH, so that is what these assert.
// ------------------------------------------------------------------------------------------------

const gRow = (id: string, depth: number): FlatRow => ({
  kind: 'group',
  depth,
  group: { id, name: id } as unknown as TreeGroup,
  isOpen: true,
  hasChildren: true,
  tally: null,
});
const nRow = (id: string, depth: number, kind: 'node' | 'ungrouped-node' = 'node'): FlatRow =>
  ({ kind, depth, node: { id, name: id } as unknown as NodeSummary }) as FlatRow;

/**
 * The screenshot that motivated the increment, as rows:
 *
 *     0  Internet Sites            depth 0
 *     1    DNS                     depth 1
 *     2      google.com            depth 2
 *     3      test.horryworks.net   depth 2
 *     4      wg.horryworks.net     depth 2
 *     5    ping                    depth 1
 *     6      cloudflare-dns        depth 2
 *     7      (still loading)       depth 2
 *     8  Ungrouped
 *     9    loose                   depth 1
 */
const SCREEN: readonly FlatRow[] = [
  gRow('sites', 0),
  gRow('dns', 1),
  nRow('google', 2),
  nRow('test', 2),
  nRow('wg', 2),
  gRow('ping', 1),
  nRow('cloudflare', 2),
  { kind: 'group-loading', depth: 2, groupId: 'ping' },
  { kind: 'ungrouped-head', count: 1 },
  nRow('loose', 1, 'ungrouped-node'),
];

const feedback = (target: Target | 'root', position: DropPos, ok = true): DropFeedback => ({
  target,
  position,
  ok,
});

/** Where the slot ended up and at what depth — with -1 for "there is no slot", so a test that means
 *  to find one cannot pass by finding nothing. */
const slotAt = (rows: readonly FlatRow[]) => {
  const index = rows.findIndex((r) => r.kind === 'drop-slot');
  const row = rows[index];
  return { index, depth: row && row.kind === 'drop-slot' ? row.depth : -1 };
};

describe('withDropSlot', () => {
  it('puts a folder dropped below a folder’s last node INSIDE that folder', () => {
    // The red line in the report. `wg` is the last row `DNS` contains, so landing after it means
    // becoming its sibling — a member of `DNS`, drawn at the members' own depth.
    const rows = withDropSlot(SCREEN, feedback(nodeTarget('wg', 'dns'), 'after'));
    expect(slotAt(rows)).toEqual({ index: 5, depth: 2 });
  });

  it('puts a folder dropped above the next folder BESIDE it, one level up', () => {
    // Five pixels lower on screen, and a different parent. Note the index: **the same one**. The
    // depth is the entire difference, which is why the indentation had to become the indicator.
    const rows = withDropSlot(SCREEN, feedback(groupTarget('ping', 'sites'), 'before'));
    expect(slotAt(rows)).toEqual({ index: 5, depth: 1 });
  });

  it('steps over everything a folder contains when the drop is after the folder', () => {
    // Dropping after `DNS` makes the dragged item DNS's next sibling, so the slot belongs below the
    // last row DNS holds. Putting it at index 2 — directly under the folder's own row — would draw
    // "inside DNS" for a placement that is not.
    const rows = withDropSlot(SCREEN, feedback(groupTarget('dns', 'sites'), 'after'));
    expect(slotAt(rows)).toEqual({ index: 5, depth: 1 });
  });

  it('stops at the Ungrouped header instead of swallowing it into the folder above', () => {
    // 🚨 The header has no depth of its own and `rowDepth` answers -1 for it. A copy of that helper
    // answering 0 would let this walk run past the header and off the end of the list, putting the
    // slot below every ungrouped node — a placement the drop does not make.
    const rows = withDropSlot(SCREEN, feedback(groupTarget('ping', 'sites'), 'after'));
    expect(slotAt(rows)).toEqual({ index: 8, depth: 1 });
  });

  it('finds the folder’s own row, not the loading placeholder standing under it', () => {
    // Both rows answer to `ping`. `flatRowKey` is what tells them apart (`g:ping` vs `loading:ping`)
    // — a lookup on a bare id would match the placeholder at index 7 and draw the slot in the wrong
    // parent, at the wrong depth, with nothing on screen to say it had.
    const rows = withDropSlot(SCREEN, feedback(groupTarget('ping', 'sites'), 'before'));
    expect(slotAt(rows)).toEqual({ index: 5, depth: 1 });
  });

  it('places a node dropped among the Ungrouped rows there', () => {
    const rows = withDropSlot(SCREEN, feedback(nodeTarget('loose', null), 'before'));
    expect(slotAt(rows)).toEqual({ index: 9, depth: 1 });
  });

  it('keeps every original row, in order, and adds exactly one slot', () => {
    // ⚠️ The slot is ADDED, never a MOVE: the dragged rows stay where they are. Lifting a folder's
    // subtree out would shift the rows below it by as many rows as the subtree is deep, which
    // changes what is under the cursor — and re-judging from there moves the slot again.
    // ⚠️ Exactly one also matters mechanically: `flatRowKey` answers a constant for the slot, so a
    // second one would collide in the virtualizer's `getItemKey` and in React's key.
    const rows = withDropSlot(SCREEN, feedback(nodeTarget('wg', 'dns'), 'after'));
    expect(rows).toHaveLength(SCREEN.length + 1);
    expect(rows.filter((r) => r.kind === 'drop-slot')).toHaveLength(1);
    expect(rows.filter((r) => r.kind !== 'drop-slot')).toEqual([...SCREEN]);
  });

  it('shows nothing for “into this folder”, which the target row already outlines', () => {
    // `inside` appends to the end of the folder's contents, which is routinely off screen. A slot
    // the operator cannot see is worse than the outline drawn where they ARE looking.
    expect(withDropSlot(SCREEN, feedback(groupTarget('dns', 'sites'), 'inside'))).toBe(SCREEN);
  });

  it('shows nothing for a refused drop, for the Ungrouped header, and for no drag at all', () => {
    expect(withDropSlot(SCREEN, feedback(nodeTarget('wg', 'dns'), 'after', false))).toBe(SCREEN);
    expect(withDropSlot(SCREEN, feedback('root', 'inside'))).toBe(SCREEN);
    expect(withDropSlot(SCREEN, null)).toBe(SCREEN);
  });

  it('returns the very same array when the target row is not on screen', () => {
    // Not merely equal — the same reference. An idle tree renders from exactly the array
    // `flattenTree` produced, so this increment costs a tree nobody is dragging over nothing.
    expect(withDropSlot(SCREEN, feedback(nodeTarget('nobody', 'dns'), 'after'))).toBe(SCREEN);
  });
});

describe('dropParentId', () => {
  it('names the folder the drop writes into', () => {
    // The explicit half of the answer: the slot's indent implies `DNS`, this marks its row.
    expect(dropParentId(feedback(nodeTarget('wg', 'dns'), 'after'))).toBe('dns');
    expect(dropParentId(feedback(groupTarget('ping', 'sites'), 'before'))).toBe('sites');
  });

  it('names nothing when there is no folder row standing for the destination', () => {
    // Top level and the Ungrouped bucket are real destinations with no row of their own, so there
    // the slot's indentation is the only mark — which is not the same as saying "refused".
    expect(dropParentId(feedback(nodeTarget('loose', null), 'before'))).toBeNull();
    expect(dropParentId(feedback('root', 'inside'))).toBeNull();
  });

  it('names nothing while the target row itself is the destination, or the drop is refused', () => {
    // `inside` already outlines the target row; marking it twice would say two different things
    // about one row.
    expect(dropParentId(feedback(groupTarget('dns', 'sites'), 'inside'))).toBeNull();
    expect(dropParentId(feedback(nodeTarget('wg', 'dns'), 'after', false))).toBeNull();
    expect(dropParentId(null)).toBeNull();
  });
});

describe('dragPreview', () => {
  it('names the folder being dragged', () => {
    expect(dragPreview(SCREEN, GROUPS, groupDrag('rack'))).toEqual({
      kind: 'group',
      group: GROUPS[1],
    });
  });

  it('names the grabbed node and counts the others travelling with it', () => {
    // ⚠️ The grabbed row, not the first of the batch — the operator checks three rows and then
    // grabs whichever one the pointer is over. A slot naming one row of three understates the move,
    // which is the same defect ADR-124 Inc.4 fixed one level down.
    expect(dragPreview(SCREEN, GROUPS, grabbed('test', 'google', 'test', 'wg'))).toEqual({
      kind: 'node',
      name: 'test',
      extra: 2,
    });
  });

  it('finds a node drawn under Ungrouped as readily as one inside a folder', () => {
    expect(dragPreview(SCREEN, GROUPS, nodeDrag('loose'))).toEqual({
      kind: 'node',
      name: 'loose',
      extra: 0,
    });
  });

  it('answers null when the grabbed row is not among the rows in hand', () => {
    // A folder collapsed mid-drag. The slot is still drawn — WHERE the drop lands is the answer it
    // exists to give, and that does not stop being true because the name is momentarily unavailable.
    expect(dragPreview(SCREEN, GROUPS, nodeDrag('gone'))).toBeNull();
    expect(dragPreview(SCREEN, GROUPS, groupDrag('gone'))).toBeNull();
    expect(dragPreview(SCREEN, GROUPS, null)).toBeNull();
  });
});

describe('dropToPerform', () => {
  const folder = { kind: 'group', id: 'F', scope: 'P' } as const;
  const below = { kind: 'node', id: 'n2', scope: 'P' } as const;

  it('performs what was SHOWN, not what the row under the pointer says now', () => {
    // The defect: the slot above the folder is removed when the judgement turns `inside`, the
    // folder jumps up a row, and the drop event lands on the node that slid under the pointer.
    const shown = { target: folder, position: 'inside', ok: true } as const;
    const judgedNow = { target: below, position: 'before', ok: true } as const;
    expect(dropToPerform(shown, judgedNow)).toEqual({ target: folder, position: 'inside' });
  });

  it('keeps a refused placement refused, whatever the row under the pointer would allow', () => {
    const shown = { target: folder, position: 'inside', ok: false } as const;
    const judgedNow = { target: below, position: 'before', ok: true } as const;
    expect(dropToPerform(shown, judgedNow)).toBeNull();
  });

  it('falls back to the row when nothing was shown, or what was shown belongs to the Ungrouped header', () => {
    const judgedNow = { target: below, position: 'after', ok: true } as const;
    expect(dropToPerform(null, judgedNow)).toEqual({ target: below, position: 'after' });
    expect(dropToPerform({ target: 'root', position: 'inside', ok: true }, judgedNow)).toEqual({
      target: below,
      position: 'after',
    });
    expect(dropToPerform(null, { ...judgedNow, ok: false })).toBeNull();
  });
});
