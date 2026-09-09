// SPDX-License-Identifier: AGPL-3.0-only
// The working-set rules (ADR-124). Every case here is one the tree can reach with two clicks, and
// three of them are the ones that would move nodes nobody picked.
import { describe, expect, it } from 'vitest';
import { clickGesture, clickOutcome, rangeChecked, rowNode, toggleChecked } from './nodeTreeSelect';
import type { FlatRow } from '../../lib/nodeTree';
import type { NodeSummary } from '../../types/api';

const node = (id: string): NodeSummary => ({ id, name: id }) as NodeSummary;

const nodeRow = (id: string): FlatRow => ({ kind: 'node', depth: 1, node: node(id) });
const ungroupedRow = (id: string): FlatRow => ({
  kind: 'ungrouped-node',
  depth: 1,
  node: node(id),
});
const groupRow = (id: string): FlatRow =>
  ({ kind: 'group', depth: 0, group: { id }, isOpen: true, hasChildren: true }) as FlatRow;
const loadingRow = (): FlatRow => ({ kind: 'group-loading', depth: 1, groupId: 'g9' });

const checked = (...ids: string[]) => new Map(ids.map((id) => [id, node(id)]));

describe('toggleChecked', () => {
  it('adds a node that is not in the set and removes one that is', () => {
    const one = toggleChecked(new Map(), node('a'));
    expect([...one.keys()]).toEqual(['a']);
    expect([...toggleChecked(one, node('a')).keys()]).toEqual([]);
  });

  it('returns a new map rather than mutating the old one', () => {
    // The page holds this in React state, so an in-place edit renders nothing.
    const before = checked('a');
    const after = toggleChecked(before, node('b'));
    expect([...before.keys()]).toEqual(['a']);
    expect([...after.keys()]).toEqual(['a', 'b']);
  });
});

describe('rangeChecked', () => {
  const flat = [groupRow('g1'), nodeRow('a'), nodeRow('b'), nodeRow('c'), nodeRow('d')];

  it('takes both ends and everything between them', () => {
    const r = rangeChecked(flat, 'b', node('d'), new Map());
    expect([...r!.checked.keys()]).toEqual(['b', 'c', 'd']);
    expect(r!.anchorId).toBe('b');
  });

  it('works upwards as well as downwards', () => {
    const r = rangeChecked(flat, 'd', node('b'), new Map());
    expect([...r!.checked.keys()].sort()).toEqual(['b', 'c', 'd']);
  });

  it('adds to the working set instead of replacing it', () => {
    // The rule is "the set only grows"; clearing is a plain click, Escape, or the bar's button.
    const r = rangeChecked(flat, 'c', node('d'), checked('a'));
    expect([...r!.checked.keys()].sort()).toEqual(['a', 'c', 'd']);
  });

  it('skips rows that are not nodes', () => {
    // 🚨 A folder row and a lazy-load placeholder sit inside a run. Checking them would put a
    // group id into a set of node ids, which the move endpoint would report as simply missing.
    const mixed = [nodeRow('a'), groupRow('g2'), loadingRow(), ungroupedRow('z')];
    const r = rangeChecked(mixed, 'a', node('z'), new Map());
    expect([...r!.checked.keys()]).toEqual(['a', 'z']);
  });

  it('starts a new run when the anchor is no longer on screen', () => {
    // 🚨 THE ONE THAT MATTERS. The anchor's folder was collapsed (or a filter hid it, or its
    // lazily-loaded page was replaced) between the two clicks. Ranging from index 0 instead would
    // check every row above the click — a gesture meaning "these two" selecting the whole tree.
    const r = rangeChecked(flat, 'gone', node('c'), new Map());
    expect([...r!.checked.keys()]).toEqual(['c']);
    expect(r!.anchorId).toBe('c');
  });

  it('starts a new run when there is no anchor at all', () => {
    const r = rangeChecked(flat, null, node('c'), new Map());
    expect([...r!.checked.keys()]).toEqual(['c']);
    expect(r!.anchorId).toBe('c');
  });

  it('leaves the set alone when the clicked node is not in the rows', () => {
    expect(rangeChecked(flat, 'a', node('elsewhere'), new Map())).toBeNull();
  });
});

describe('rowNode', () => {
  it('answers for both node buckets and for nothing else', () => {
    expect(rowNode(nodeRow('a'))?.id).toBe('a');
    expect(rowNode(ungroupedRow('b'))?.id).toBe('b');
    expect(rowNode(groupRow('g1'))).toBeNull();
    expect(rowNode(loadingRow())).toBeNull();
    expect(rowNode({ kind: 'ungrouped-head', count: 3 })).toBeNull();
  });
});

describe('clickOutcome', () => {
  // 🚨 The block that did not exist when the feature shipped, and the reason the bug did.
  // `rangeChecked` above is exercised with an anchor the test supplies; nothing asked who
  // supplies it in the running tree, and the answer — until 増分 1 — was "nobody, until a Ctrl
  // or Shift click has already happened".
  const flat = [groupRow('g1'), nodeRow('a'), nodeRow('b'), nodeRow('c'), nodeRow('d')];
  const plain = { ctrlKey: false, metaKey: false, shiftKey: false };
  const ctrl = { ...plain, ctrlKey: true };
  const shift = { ...plain, shiftKey: true };

  it('takes the whole run when a plain click is followed by a Shift click', () => {
    // 🚨 THE REGRESSION. Reported from the running box: select `sim-arista-eos`, Shift-click
    // `sim-cisco-2960x-mau`, and the row between them stayed unselected — because the range had
    // never started. Both clicks, in order, through the same function the tree calls.
    const first = clickOutcome(plain, flat, null, node('a'), new Map());
    expect(first.anchorId, 'a plain click left no anchor for Shift to measure from').toBe('a');

    const second = clickOutcome(shift, flat, first.anchorId, node('d'), first.checked!);
    expect([...second.checked!.keys()]).toEqual(['a', 'b', 'c', 'd']);
  });

  it('anchors a plain click even though it checks nothing', () => {
    const r = clickOutcome(plain, flat, null, node('c'), new Map());
    expect([...r.checked!.keys()]).toEqual([]);
    expect(r.anchorId).toBe('c');
    expect(r.select).toBe(true);
  });

  it('abandons the batch on a plain click', () => {
    // "Never mind those" — the set goes, and the pane moves to the row that was clicked.
    const r = clickOutcome(plain, flat, 'a', node('c'), checked('a', 'b'));
    expect([...r.checked!.keys()]).toEqual([]);
    expect(r.select).toBe(true);
  });

  it('leaves the pane alone for Ctrl and Shift', () => {
    // The pane keeps showing whatever was open while a batch is assembled — Ctrl / Shift never
    // write `?sel=`, which is what keeps ADR-073's three clear gestures untouched.
    expect(clickOutcome(ctrl, flat, null, node('b'), new Map()).select).toBe(false);
    expect(clickOutcome(shift, flat, 'a', node('b'), new Map()).select).toBe(false);
  });

  it('moves the anchor to the row a Ctrl click landed on', () => {
    const r = clickOutcome(ctrl, flat, 'a', node('c'), new Map());
    expect([...r.checked!.keys()]).toEqual(['c']);
    expect(r.anchorId).toBe('c');
  });

  it('writes nothing when a Shift click lands on a node that is not among the rows', () => {
    // `checked: null` means "leave the working set exactly as it is" — distinct from an empty
    // map, which would clear it. The anchor survives so the next Shift click still has a run.
    const r = clickOutcome(shift, flat, 'a', node('elsewhere'), checked('a'));
    expect(r.checked).toBeNull();
    expect(r.anchorId).toBe('a');
    expect(r.select).toBe(false);
  });
});

describe('clickGesture', () => {
  const ev = (over: Partial<Record<'ctrlKey' | 'metaKey' | 'shiftKey', boolean>> = {}) => ({
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    ...over,
  });

  it('reads ⌘ as Ctrl', () => {
    // macOS has no Ctrl-click for this; it is ⌘-click, and Ctrl-click opens a context menu.
    expect(clickGesture(ev({ metaKey: true }))).toBe('toggle');
    expect(clickGesture(ev({ ctrlKey: true }))).toBe('toggle');
  });

  it('lets Shift win when both are held', () => {
    expect(clickGesture(ev({ shiftKey: true, ctrlKey: true }))).toBe('range');
  });

  it('leaves an unmodified click alone', () => {
    // This is what keeps ADR-073's three clear-the-selection gestures working untouched.
    expect(clickGesture(ev())).toBe('plain');
  });
});
