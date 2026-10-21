// SPDX-License-Identifier: AGPL-3.0-only
// Bringing the selection back into view when narrowing ends (ADR-073 Inc.2). Every "scroll" case
// asserts the row it lands on, not only that it scrolls — a reveal one row off passes "it scrolled".
import { describe, expect, it } from 'vitest';
import {
  anchorAt,
  anchorToSave,
  foldersToOpen,
  isRevealArrival,
  openFolders,
  readTreeAnchor,
  restoreOffset,
  restoreStep,
  revealOnNarrowing,
  REVEAL_ARRIVAL,
  REVEAL_FOLLOW_MS,
  revealHolds,
  revealOutstayed,
  revealRequestFor,
  revealStep,
  TREE_ANCHOR_KEY,
  waitsOnFilter,
  writeTreeAnchor,
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

describe('revealHolds', () => {
  // The row the reveal scrolled to is index 3: a folder above it still showing its placeholder.
  const drawn = [groupRow('above'), loadingRow('above'), groupRow('site'), nodeRow('sw9'), loadingRow('below')];
  const none = new Set<string>();

  it('stays while a folder above the row is still loading — its rows would push the row down', () => {
    expect(revealHolds(drawn, 3, new Set(['above']), none)).toBe(true);
  });

  it('stays while a placeholder above the row is on screen, before its request has gone out', () => {
    expect(revealHolds(drawn, 3, none, new Set(['above']))).toBe(true);
  });

  it('lets go once the folders above have answered', () => {
    const settled = [groupRow('above'), nodeRow('a1'), groupRow('site'), nodeRow('sw9')];
    expect(revealHolds(settled, 3, none, none)).toBe(false);
  });

  it('ignores a placeholder nobody is asking for: it stays one row until scrolled to', () => {
    expect(revealHolds(drawn, 3, new Set(['elsewhere']), new Set(['elsewhere']))).toBe(false);
  });

  it('ignores folders below the row, which cannot move it', () => {
    expect(revealHolds(drawn, 3, new Set(['below']), new Set(['below']))).toBe(false);
  });

  it('ignores a failed folder, whose row never grows', () => {
    const failed = [groupRow('above'), failedRow('above'), groupRow('site'), nodeRow('sw9')];
    expect(revealHolds(failed, 3, new Set(['above']), new Set(['above']))).toBe(false);
  });
});

describe('revealOutstayed', () => {
  it('follows the row until the limit, and no longer', () => {
    expect(revealOutstayed(1_000, 1_000)).toBe(false);
    expect(revealOutstayed(1_000, 1_000 + REVEAL_FOLLOW_MS - 1)).toBe(false);
    expect(revealOutstayed(1_000, 1_000 + REVEAL_FOLLOW_MS)).toBe(true);
  });
});

describe('putting the tree back after a reload (ADR-073 Inc.3)', () => {
  const ROW = 30;
  const memoryStorage = (): Storage => {
    const m = new Map<string, string>();
    return {
      getItem: (k: string) => m.get(k) ?? null,
      setItem: (k: string, v: string) => void m.set(k, v),
      removeItem: (k: string) => void m.delete(k),
      clear: () => m.clear(),
      key: () => null,
      get length() {
        return m.size;
      },
    };
  };
  const member = (id: string): FlatRow => ({
    kind: 'node',
    depth: 2,
    node: { id, name: id, group_id: 'site' } as NodeSummary,
  });
  const drawn: FlatRow[] = [groupRow('region'), groupRow('site', 1), loadingRow('site'), member('sw1'), member('sw2')];

  it('names the row at the top of the pane and how far its edge sat above it', () => {
    // 100px down: sw1 (row 3) starts at 90, so its edge is 10px above the pane's top.
    expect(anchorAt(drawn, 100, ROW)).toEqual({ sel: { kind: 'node', id: 'sw1' }, groupId: 'site', seq: 0, top: -10 });
  });

  it('walks past a loading placeholder to the next row a reload can find', () => {
    // 65px: row 2 is the placeholder; sw1 (row 3) starts 25px below the pane's top.
    expect(anchorAt(drawn, 65, ROW)).toMatchObject({ sel: { kind: 'node', id: 'sw1' }, top: 25 });
  });

  it('names a folder by its parent, which is what opening the way to it needs', () => {
    // `groupRow` builds a folder with no parent_id.
    expect(anchorAt(drawn, 30, ROW)).toMatchObject({ sel: { kind: 'group', id: 'site' }, groupId: null, top: 0 });
  });

  it('has nothing to restore at the very top, or with no row below the pane top', () => {
    expect(anchorAt(drawn, 0, ROW)).toBeNull();
    expect(anchorAt([], 300, ROW)).toBeNull();
    expect(anchorAt([groupRow('region'), loadingRow('region')], 30, ROW)).toBeNull();
  });

  it('keeps the stored point while a restore has not put the pane back yet', () => {
    // A second reload before the rows arrive: the pane is still at the top, which is not where the
    // operator left it, so nothing is written over the point.
    expect(anchorToSave(true, drawn, 0, ROW)).toBeUndefined();
    expect(anchorToSave(false, drawn, 0, ROW)).toBeNull();
    expect(anchorToSave(false, drawn, 100, ROW)).toEqual(anchorAt(drawn, 100, ROW));
  });

  it('scrolls so the row sits where it sat, never above the top of the tree', () => {
    expect(restoreOffset(3, ROW, -10)).toBe(100);
    expect(restoreOffset(3, ROW, 25)).toBe(65);
    expect(restoreOffset(0, ROW, 25)).toBe(0);
  });

  it('lands on the anchor row at its index NOW, and waits until it is drawn', () => {
    const req = anchorAt(drawn, 100, ROW)!;
    expect(restoreStep(drawn, req)).toEqual({ kind: 'scroll', index: 3 });
    // The folder above answered differently after the reload: the row moved, the request follows it.
    expect(restoreStep([drawn[0], drawn[1], drawn[3], drawn[4]], req)).toEqual({ kind: 'scroll', index: 2 });
    expect(restoreStep([drawn[0], drawn[1], drawn[2]], req)).toEqual({ kind: 'wait' });
  });

  it('round-trips through the tab storage, and forgets on null', () => {
    const store = memoryStorage();
    writeTreeAnchor(store, anchorAt(drawn, 100, ROW));
    expect(readTreeAnchor(store)).toEqual({ sel: { kind: 'node', id: 'sw1' }, groupId: 'site', seq: -1, top: -10 });
    writeTreeAnchor(store, null);
    expect(readTreeAnchor(store)).toBeNull();
    expect(store.length).toBe(0);
  });

  it('ignores a stored value it did not write, and storage that is missing or throws', () => {
    const store = memoryStorage();
    const bad = [
      '{',
      '{"sel":{"kind":"x","id":"a"},"groupId":null,"top":0}',
      '{"sel":{"kind":"node","id":"a"},"groupId":3,"top":0}',
      '{"sel":{"kind":"node","id":"a"},"groupId":null}',
    ];
    for (const raw of bad) {
      store.setItem(TREE_ANCHOR_KEY, raw);
      expect(readTreeAnchor(store)).toBeNull();
    }
    expect(readTreeAnchor(undefined)).toBeNull();
    const refusing: Storage = {
      ...memoryStorage(),
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('quota');
      },
    };
    expect(() => writeTreeAnchor(refusing, anchorAt(drawn, 100, ROW))).not.toThrow();
    expect(readTreeAnchor(refusing)).toBeNull();
  });
});

describe('a row the operator asked for (ADR-073 Inc.3)', () => {
  const asked: RevealRequest = { sel: { kind: 'group', id: 'site' }, groupId: 'region', seq: 2, asked: true };
  const rows = [groupRow('region'), groupRow('site', 1)];

  it('does not wait out a filter, while a reveal made by clearing one still does', () => {
    expect(revealStep(rows, asked, ctx({ filtering: waitsOnFilter(true, asked) }))).toEqual({
      kind: 'scroll',
      index: 1,
    });
    const unasked: RevealRequest = { sel: asked.sel, groupId: asked.groupId, seq: 2 };
    expect(revealStep(rows, unasked, ctx({ filtering: waitsOnFilter(true, unasked) }))).toEqual({ kind: 'wait' });
  });

  it('is done when the narrowed tree does not draw it — there is nothing to wait for', () => {
    expect(revealStep([groupRow('region')], asked, ctx({ filtering: waitsOnFilter(true, asked) }))).toEqual({
      kind: 'done',
    });
  });

  it('arrives from another screen only with its own mark in history state', () => {
    expect(isRevealArrival(REVEAL_ARRIVAL)).toBe(true);
    expect(isRevealArrival(null)).toBe(false);
    expect(isRevealArrival({ revealSelection: 'yes' })).toBe(false);
    expect(isRevealArrival('revealSelection')).toBe(false);
  });
});

describe('a narrowing control coming into force (ADR-073 Inc.3)', () => {
  const restore: RevealRequest = { sel: { kind: 'node', id: 'sw1' }, groupId: 'site', seq: -1, top: 12 };
  const reveal: RevealRequest = { sel: { kind: 'node', id: 'sw1' }, groupId: 'site', seq: 3 };

  it('keeps a restore when the tree loaded narrowed, or when Pinned only took effect as the pins arrived', () => {
    expect(revealOnNarrowing(restore, true, false)).toBe(restore);
    // A reload with Pinned only on: the switch counts only once the pins load, a render later.
    expect(revealOnNarrowing(restore, false, true)).toBe(restore);
  });

  it('ends a restore when the operator starts to narrow, and any other reveal always', () => {
    expect(revealOnNarrowing(restore, false, false)).toBeNull();
    expect(revealOnNarrowing(reveal, true, true)).toBeNull();
    expect(revealOnNarrowing(null, true, true)).toBeNull();
  });
});
