// SPDX-License-Identifier: AGPL-3.0-only
// Bringing the selection back into view when narrowing ends (ADR-073 増分 2). Every "scroll" case
// asserts the row it lands on, not only that it scrolls — a reveal one row off passes "it scrolled".
import { describe, expect, it } from 'vitest';
import {
  foldersToOpen,
  openFolders,
  revealRequestFor,
  revealStep,
  type RevealRequest,
} from './nodeTreeReveal';
import { UNGROUPED, type FlatRow } from '../../lib/nodeTree';
import type { NodeGroup, NodeSummary } from '../../types/api';

const group = (id: string, parent: string | null = null): NodeGroup =>
  ({ id, name: id, parent_id: parent }) as NodeGroup;
const GROUPS = [group('region'), group('site', 'region'), group('rack', 'site')];

const node = (id: string): NodeSummary => ({ id, name: id }) as NodeSummary;
const nodeRow = (id: string, depth = 1): FlatRow => ({ kind: 'node', depth, node: node(id) });
const groupRow = (id: string, depth = 0): FlatRow =>
  ({ kind: 'group', depth, group: { id }, isOpen: true, hasChildren: true, tally: null }) as unknown as FlatRow;
const loadingRow = (groupId: string): FlatRow => ({ kind: 'group-loading', depth: 1, groupId });
const failedRow = (groupId: string): FlatRow => ({ kind: 'group-failed', depth: 1, groupId });

const nodeReq = (groupId: string | null): RevealRequest => ({
  sel: { kind: 'node', id: 'sw9' },
  groupId,
  seq: 1,
});
const ctx = (over: Partial<Parameters<typeof revealStep>[2]> = {}) => ({
  filtering: false,
  collapsed: {},
  folders: ['region', 'site'],
  loadedGroups: new Set<string>(['region', 'site']),
  ...over,
});

describe('revealRequestFor', () => {
  it('asks for a node in the folder its row was seen in', () => {
    expect(revealRequestFor({ kind: 'node', id: 'n' }, GROUPS, 'site', 3)).toEqual({
      sel: { kind: 'node', id: 'n' },
      groupId: 'site',
      seq: 3,
    });
  });

  it('asks for an Ungrouped node with a null folder', () => {
    expect(revealRequestFor({ kind: 'node', id: 'n' }, GROUPS, null, 1)?.groupId).toBeNull();
  });

  it('asks nothing for a node whose row was never seen — guessing would scroll somewhere wrong', () => {
    expect(revealRequestFor({ kind: 'node', id: 'n' }, GROUPS, undefined, 1)).toBeNull();
  });

  it('asks for a folder under its parent, whose chain is what has to open', () => {
    expect(revealRequestFor({ kind: 'group', id: 'rack' }, GROUPS, undefined, 1)?.groupId).toBe('site');
    expect(revealRequestFor({ kind: 'group', id: 'region' }, GROUPS, undefined, 1)?.groupId).toBeNull();
  });

  it('asks nothing with no selection, or for a folder that no longer exists', () => {
    expect(revealRequestFor(null, GROUPS, 'site', 1)).toBeNull();
    expect(revealRequestFor({ kind: 'group', id: 'gone' }, GROUPS, undefined, 1)).toBeNull();
  });
});

describe('foldersToOpen / openFolders', () => {
  it('opens the whole chain from the root down, the folder itself included', () => {
    expect(foldersToOpen(GROUPS, 'rack')).toEqual(['region', 'site', 'rack']);
    expect(foldersToOpen(GROUPS, null)).toEqual([]);
  });

  it('takes only the chain out of the saved layout and keeps every other closed folder', () => {
    const next = openFolders({ region: true, rack: true, other: true }, ['region', 'site', 'rack']);
    expect(next).toEqual({ other: true });
  });

  it('hands back the same object when nothing on the chain was closed, so nothing is saved', () => {
    const collapsed = { other: true as const };
    expect(openFolders(collapsed, ['region', 'site'])).toBe(collapsed);
  });
});

describe('revealStep', () => {
  const DRAWN: FlatRow[] = [groupRow('region'), groupRow('site', 1), nodeRow('sw1', 2), nodeRow('sw9', 2)];

  it('scrolls to the selected row once it is drawn', () => {
    expect(revealStep(DRAWN, nodeReq('site'), ctx())).toEqual({ kind: 'scroll', index: 3 });
  });

  it('waits while the tree is still narrowed — the search row is about to move', () => {
    expect(revealStep(DRAWN, nodeReq('site'), ctx({ filtering: true }))).toEqual({ kind: 'wait' });
  });

  it('waits while a folder on the chain is still closed, even if it is loaded', () => {
    const drawn = [groupRow('region'), groupRow('site', 1)];
    expect(revealStep(drawn, nodeReq('site'), ctx({ collapsed: { site: true } }))).toEqual({
      kind: 'wait',
    });
  });

  it('waits while the folder is still loading', () => {
    const drawn = [groupRow('region'), groupRow('site', 1), loadingRow('site')];
    expect(revealStep(drawn, nodeReq('site'), ctx({ loadedGroups: new Set(['region']) }))).toEqual({
      kind: 'wait',
    });
  });

  it('scrolls to the retry row of a folder whose fetch failed', () => {
    const drawn = [groupRow('region'), groupRow('site', 1), failedRow('site')];
    expect(revealStep(drawn, nodeReq('site'), ctx({ loadedGroups: new Set() }))).toEqual({
      kind: 'scroll',
      index: 2,
    });
  });

  it('falls back to the folder row when the folder loaded without the node', () => {
    const drawn = [groupRow('region'), groupRow('site', 1), nodeRow('sw1', 2)];
    expect(revealStep(drawn, nodeReq('site'), ctx())).toEqual({ kind: 'scroll', index: 1 });
  });

  it('gives up on an Ungrouped node that is not there once Ungrouped has loaded', () => {
    const drawn: FlatRow[] = [{ kind: 'ungrouped-head', count: 0 }];
    expect(
      revealStep(drawn, nodeReq(null), ctx({ folders: [], loadedGroups: new Set([UNGROUPED]) })),
    ).toEqual({ kind: 'done' });
  });

  it('waits for Ungrouped to load under its own key', () => {
    const drawn: FlatRow[] = [{ kind: 'ungrouped-head', count: 0 }];
    expect(revealStep(drawn, nodeReq(null), ctx({ folders: [], loadedGroups: new Set() }))).toEqual({
      kind: 'wait',
    });
  });

  it('scrolls to a selected folder, and gives up on one that is not drawn', () => {
    const req: RevealRequest = { sel: { kind: 'group', id: 'site' }, groupId: 'region', seq: 1 };
    expect(revealStep(DRAWN, req, ctx({ folders: ['region'] }))).toEqual({ kind: 'scroll', index: 1 });
    expect(revealStep([groupRow('region')], req, ctx({ folders: ['region'] }))).toEqual({
      kind: 'done',
    });
  });
});
