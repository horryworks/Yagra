// SPDX-License-Identifier: AGPL-3.0-only
// The tree's drop judgement — four branches choosing which of four writes fires and with which
// arguments, and until ADR-052 Inc.6 they were in `NodeTree.tsx`, which Vitest never loads.
//
// What makes this worth testing is the failure shape: a wrong branch does not throw and does not
// look broken. It moves a node into a different group, the write succeeds, and the only person who
// finds out is whoever later wonders why their device is filed somewhere else.
import { describe, expect, it } from 'vitest';
import { dropAction, dropAllowed, dropPosition, rootDropAction, type Target } from './nodeTreeDnd';
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

describe('dropPosition', () => {
  it('always nests a node dropped on a group', () => {
    // A node cannot be a sibling of a group, so the before/after bands would offer a placement
    // that has no meaning.
    for (const y of [0, 5, 15, 29]) expect(dropPosition(y, 30, true, 'node')).toBe('inside');
  });

  it('splits a group row into quarter / half / quarter', () => {
    expect(dropPosition(0, 40, true, 'group')).toBe('before');
    expect(dropPosition(9, 40, true, 'group')).toBe('before');
    expect(dropPosition(10, 40, true, 'group')).toBe('inside'); // exactly 25% is already inside
    expect(dropPosition(20, 40, true, 'group')).toBe('inside');
    expect(dropPosition(30, 40, true, 'group')).toBe('inside'); // exactly 75% is still inside
    expect(dropPosition(31, 40, true, 'group')).toBe('after');
  });

  it('splits a node row in half — there is no inside a node', () => {
    expect(dropPosition(0, 30, false, 'group')).toBe('before');
    expect(dropPosition(14, 30, false, 'group')).toBe('before');
    expect(dropPosition(15, 30, false, 'group')).toBe('after'); // exactly half is after
    expect(dropPosition(29, 30, false, 'node')).toBe('after');
  });

  it('does not divide by a zero height', () => {
    // 🚨 A row measured mid-layout reports 0. Dividing by it makes every comparison NaN, which is
    // false — so every drop would silently read `after`, on both kinds of row.
    expect(dropPosition(0, 0, true, 'group')).toBe('before');
    expect(dropPosition(0, 0, false, 'group')).toBe('before');
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
    const n = { kind: 'node' as const, id: 'n1' };
    expect(dropAllowed(GROUPS, n, groupTarget('site'), 'inside')).toBe(true);
    expect(dropAllowed(GROUPS, n, nodeTarget('n2', 'site'), 'before')).toBe(true);
  });

  it('refuses a node dropped on itself', () => {
    const n = { kind: 'node' as const, id: 'n1' };
    expect(dropAllowed(GROUPS, n, nodeTarget('n1', 'site'), 'before')).toBe(false);
  });

  it('refuses a group dropped on a node, or on itself', () => {
    const g = { kind: 'group' as const, id: 'site' };
    expect(dropAllowed(GROUPS, g, nodeTarget('n1', 'site'), 'before')).toBe(false);
    expect(dropAllowed(GROUPS, g, groupTarget('site'), 'inside')).toBe(false);
  });

  it('refuses nesting a group inside its own subtree', () => {
    // The cycle guard. Allowing it orphans every descendant.
    const g = { kind: 'group' as const, id: 'site' };
    expect(dropAllowed(GROUPS, g, groupTarget('rack', 'site'), 'inside')).toBe(false);
    expect(dropAllowed(GROUPS, g, groupTarget('shelf', 'rack'), 'inside')).toBe(false);
  });

  it('refuses re-parenting a group BESIDE one of its own descendants', () => {
    // 🚨 The subtle half. `before`/`after` re-parents to the target's SCOPE, so dropping `site`
    // beside `shelf` would put `site` inside `rack` — the same cycle, one level up. Checking the
    // target rather than its scope here would let it through.
    const g = { kind: 'group' as const, id: 'site' };
    expect(dropAllowed(GROUPS, g, groupTarget('shelf', 'rack'), 'before')).toBe(false);
    expect(dropAllowed(GROUPS, g, groupTarget('rack', 'site'), 'after')).toBe(false);
  });

  it('permits a group moving to the top level or under an unrelated one', () => {
    const g = { kind: 'group' as const, id: 'rack' };
    expect(dropAllowed(GROUPS, g, groupTarget('other', null), 'before')).toBe(true);
    expect(dropAllowed(GROUPS, g, groupTarget('other'), 'inside')).toBe(true);
  });
});

describe('dropAction', () => {
  it('assigns a node into the group it was dropped on', () => {
    expect(dropAction({ kind: 'node', id: 'n1' }, groupTarget('site'), 'inside')).toEqual({
      kind: 'move-node',
      nodeId: 'n1',
      groupId: 'site',
    });
  });

  it('orders a node against a sibling, in the TARGET’s group', () => {
    // 🚨 `groupId` is the target's scope, not the dragged node's. Dropping a node beside a node in
    // another group both moves and orders it; reading the dragged node's own group would leave it
    // where it was while claiming to have moved it.
    expect(dropAction({ kind: 'node', id: 'n1' }, nodeTarget('n2', 'rack'), 'before')).toEqual({
      kind: 'reorder-node',
      nodeId: 'n1',
      groupId: 'rack',
      before: 'n2',
    });
    expect(dropAction({ kind: 'node', id: 'n1' }, nodeTarget('n2', 'rack'), 'after')).toEqual({
      kind: 'reorder-node',
      nodeId: 'n1',
      groupId: 'rack',
      after: 'n2',
    });
  });

  it('nests a group when dropped in the middle, and re-parents it when dropped on an edge', () => {
    expect(dropAction({ kind: 'group', id: 'rack' }, groupTarget('other'), 'inside')).toEqual({
      kind: 'move-group',
      groupId: 'rack',
      parentId: 'other',
    });
    expect(dropAction({ kind: 'group', id: 'rack' }, groupTarget('other', null), 'before')).toEqual({
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
      const a = dropAction({ kind: 'node', id: 'n1' }, nodeTarget('n2', null), pos);
      expect('before' in a && 'after' in a).toBe(false);
    }
  });
});

describe('rootDropAction', () => {
  it('moves either kind to the top level', () => {
    expect(rootDropAction({ kind: 'node', id: 'n1' })).toEqual({
      kind: 'move-node',
      nodeId: 'n1',
      groupId: null,
    });
    expect(rootDropAction({ kind: 'group', id: 'rack' })).toEqual({
      kind: 'move-group',
      groupId: 'rack',
      parentId: null,
    });
  });
});
