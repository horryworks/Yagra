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
  dropAction,
  dropAllowed,
  dropPosition,
  nodeDragItem,
  rootDropAction,
  type DragItem,
  type Target,
} from './nodeTreeDnd';
import type { NodeGroup } from '../../types/api';

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

  it('gives a batch over a node row no insertion point at all', () => {
    // 🚨 Inc.4 決定 C. There is no bulk placement endpoint, and calling the single one N times is a
    // write that can fail halfway with nothing to read. So a batch appends into that row's folder,
    // and the indicator says so — a row outline, never an insertion line.
    for (const y of [0, 1, 14, 15, 29]) {
      expect(dropPosition(y, 30, false, nodeDrag('n1', 'n2', 'n3'))).toBe('inside');
    }
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
    // The half Inc.4 must not take away: dragging a single node between two rows still places it
    // there. Only a batch loses the insertion point.
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
    const batch = nodeDrag('n1', 'n2', 'n3');
    expect(dropAllowed(GROUPS, batch, nodeTarget('n2', 'site'), 'inside')).toBe(false);
    expect(dropAllowed(GROUPS, batch, nodeTarget('n3', 'site'), 'inside')).toBe(false);
    // A row outside the batch is a destination like any other.
    expect(dropAllowed(GROUPS, batch, nodeTarget('n9', 'site'), 'inside')).toBe(true);
    expect(dropAllowed(GROUPS, batch, groupTarget('site'), 'inside')).toBe(true);
  });

  it('refuses a group dropped on a node, or on itself', () => {
    const g = groupDrag('site');
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', 'site'), 'before')).toBe(false);
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

  it('puts a batch dropped on a node row into that row’s folder', () => {
    // Not "into that node", which is not a thing — into the folder it sits in, appended. `scope`
    // is that folder, and `null` is the top level.
    expect(dropAction(nodeDrag('n1', 'n2'), nodeTarget('n9', 'rack'), 'inside')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1', 'n2'],
      groupId: 'rack',
    });
    expect(dropAction(nodeDrag('n1', 'n2'), nodeTarget('n9', null), 'inside')).toEqual({
      kind: 'move-nodes',
      nodeIds: ['n1', 'n2'],
      groupId: null,
    });
  });

  it('orders ONE node against a sibling, in the TARGET’s group', () => {
    // 🚨 `groupId` is the target's scope, not the dragged node's. Dropping a node beside a node in
    // another group both moves and orders it; reading the dragged node's own group would leave it
    // where it was while claiming to have moved it.
    expect(dropAction(nodeDrag('n1'), nodeTarget('n2', 'rack'), 'before')).toEqual({
      kind: 'reorder-node',
      nodeId: 'n1',
      groupId: 'rack',
      before: 'n2',
    });
    expect(dropAction(nodeDrag('n1'), nodeTarget('n2', 'rack'), 'after')).toEqual({
      kind: 'reorder-node',
      nodeId: 'n1',
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
