// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  nothingPinned,
  pinnedGroupShown,
  pinnedNodeShown,
  pinnedView,
  withMember,
} from './pins';
import type { NodeGroup } from '../types/api';

const group = (id: string, parent: string | null = null): NodeGroup =>
  ({ id, name: id, parent_id: parent }) as NodeGroup;

// Japan ─┬─ Tokyo ── Rack
//        └─ Osaka
// US
const GROUPS = [
  group('japan'),
  group('tokyo', 'japan'),
  group('rack', 'tokyo'),
  group('osaka', 'japan'),
  group('us'),
];

const ids = (...xs: string[]) => new Set(xs);

describe('pinnedView', () => {
  it('shows a pinned folder whole and the folders above it as a path', () => {
    const v = pinnedView(GROUPS, ids('tokyo'), ids(), []);
    expect([...v.subtree].sort()).toEqual(['rack', 'tokyo']);
    expect([...v.ancestors]).toEqual(['japan']);
    expect(pinnedGroupShown(v, 'osaka')).toBe(false);
    expect(pinnedGroupShown(v, 'us')).toBe(false);
  });

  it('draws a pinned node\'s path from the server row, including its own folder', () => {
    const v = pinnedView(GROUPS, ids(), ids('n1'), [{ id: 'n1', group_id: 'rack' }]);
    expect(v.subtree.size).toBe(0);
    expect([...v.ancestors].sort()).toEqual(['japan', 'rack', 'tokyo']);
    expect(pinnedNodeShown(v, { id: 'n1', group_id: 'rack' })).toBe(true);
    // A sibling in the same folder is not pinned and not inside a pinned folder.
    expect(pinnedNodeShown(v, { id: 'n2', group_id: 'rack' })).toBe(false);
  });

  it('keeps every node inside a pinned folder, however deep', () => {
    const v = pinnedView(GROUPS, ids('japan'), ids(), []);
    expect(pinnedNodeShown(v, { id: 'x', group_id: 'rack' })).toBe(true);
    expect(pinnedNodeShown(v, { id: 'y', group_id: 'us' })).toBe(false);
    expect(pinnedNodeShown(v, { id: 'z', group_id: null })).toBe(false);
  });

  it('does not call a folder inside a pinned folder an ancestor', () => {
    // Rack is pinned inside pinned Japan: Tokyo is part of Japan's subtree, so it is shown whole,
    // not reduced to a path.
    const v = pinnedView(GROUPS, ids('japan', 'rack'), ids(), []);
    expect(v.ancestors.size).toBe(0);
    expect(v.subtree.has('tokyo')).toBe(true);
  });

  it('ignores a pinned folder the caller no longer has', () => {
    const v = pinnedView(GROUPS, ids('deleted'), ids(), []);
    expect(v.subtree.size + v.ancestors.size).toBe(0);
    // Still counted as a pin, so the empty note does not claim nothing is pinned.
    expect(nothingPinned(v)).toBe(false);
  });

  it('ignores a server row whose node is no longer pinned', () => {
    // The store drops the id at once and re-reads the rows afterwards; in between, the stale row
    // must not keep drawing a path.
    const v = pinnedView(GROUPS, ids(), ids(), [{ id: 'n1', group_id: 'rack' }]);
    expect(v.ancestors.size).toBe(0);
  });

  it('survives a cycle in the folder data', () => {
    const cyclic = [group('a', 'b'), group('b', 'a')];
    const v = pinnedView(cyclic, ids(), ids('n'), [{ id: 'n', group_id: 'a' }]);
    expect([...v.ancestors].sort()).toEqual(['a', 'b']);
  });

  it('says nothing is pinned only when there is no pin at all', () => {
    expect(nothingPinned(pinnedView(GROUPS, ids(), ids(), []))).toBe(true);
    expect(nothingPinned(pinnedView(GROUPS, ids(), ids('n'), []))).toBe(false);
  });
});

describe('withMember', () => {
  it('adds and removes without touching the original', () => {
    const before = ids('a');
    const added = withMember(before, 'b', true);
    const removed = withMember(added, 'a', false);
    expect([...added].sort()).toEqual(['a', 'b']);
    expect([...removed]).toEqual(['b']);
    expect([...before]).toEqual(['a']);
  });
});
