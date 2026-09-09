// SPDX-License-Identifier: AGPL-3.0-only
// The working-set rules (ADR-124). Every case here is one the tree can reach with two clicks, and
// three of them are the ones that would move nodes nobody picked.
import { describe, expect, it } from 'vitest';
import {
  actsOnSelection,
  clickGesture,
  clickOutcome,
  rangeChecked,
  rowNode,
  toggleChecked,
} from './nodeTreeSelect';
import type { ClickContext } from './nodeTreeSelect';
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

describe('actsOnSelection', () => {
  // 🚨 The rule the right-click menu, the hover ↗ and the drag all ask (Inc.4). It lived inside
  // `nodeMoveItems` and the drag answered it by not asking, which is how a three-row selection
  // moved one node twice — once through the menu (Inc.2) and once through the drag (Inc.4).
  it('is true for a row inside a set of more than one', () => {
    expect(actsOnSelection(checked('a', 'b', 'c'), 'b')).toBe(true);
  });

  it('is false for a set of just that row — there is nothing else to carry', () => {
    expect(actsOnSelection(checked('a'), 'a')).toBe(false);
  });

  it('is false for a row outside the set, however big the set is', () => {
    // The gesture belongs to the row the operator actually acted on. Reading this the other way
    // would move a batch the pointer never touched.
    expect(actsOnSelection(checked('a', 'b', 'c'), 'z')).toBe(false);
  });

  it('is false when nothing is checked', () => {
    expect(actsOnSelection(new Map(), 'a')).toBe(false);
  });
});

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
  // 🚨 The block that did not exist when the feature shipped, and the reason two bugs did.
  // `rangeChecked` above is exercised with an anchor the test supplies; nothing asked who supplies
  // it in the running tree, and the answer — until 増分 1 — was "nobody, until a Ctrl or Shift
  // click has already happened". 増分 3 is the other half of the same omission: the batch a Ctrl
  // click adds to.
  const flat = [groupRow('g1'), nodeRow('a'), nodeRow('b'), nodeRow('c'), nodeRow('d')];
  const plain = { ctrlKey: false, metaKey: false, shiftKey: false };
  const ctrl = { ...plain, ctrlKey: true };
  const shift = { ...plain, shiftKey: true };

  /** A context over the tree's rows with nothing else set, so each test names only its own case. */
  const ctx = (over: Partial<ClickContext> = {}): ClickContext => ({
    flat,
    anchorId: null,
    selection: null,
    checked: new Map(),
    ...over,
  });

  /** The state a plain click on `id` leaves behind: the pane on that row, the anchor there, and
   *  an empty batch. Written once because every two-click test starts from it. */
  const afterPlainClick = (id: string): Partial<ClickContext> => ({
    anchorId: id,
    selection: { kind: 'node', id },
    checked: new Map(),
  });

  it('takes the whole run when a plain click is followed by a Shift click', () => {
    // 🚨 THE 増分 1 REGRESSION. Reported from the running box: select `sim-arista-eos`, Shift-click
    // `sim-cisco-2960x-mau`, and the row between them stayed unselected — because the range had
    // never started. Both clicks, in order, through the same function the tree calls.
    const first = clickOutcome(plain, node('a'), ctx());
    expect(first.anchorId, 'a plain click left no anchor for Shift to measure from').toBe('a');

    const second = clickOutcome(
      shift,
      node('d'),
      ctx({ anchorId: first.anchorId, checked: first.checked! }),
    );
    expect([...second.checked!.keys()]).toEqual(['a', 'b', 'c', 'd']);
  });

  it('keeps the first row when a plain click is followed by Ctrl clicks', () => {
    // 🚨 THE 増分 3 REGRESSION, reported with a screenshot: `sim-comware` clicked, then
    // `sim-huawei-vrp` and `sim-junos-vmx` Ctrl-clicked. Three rows painted as marked — one
    // accent bar, two tints — and two of them would move.
    const first = clickOutcome(plain, node('a'), ctx());
    const second = clickOutcome(
      ctrl,
      node('b'),
      ctx({ ...afterPlainClick('a'), checked: first.checked! }),
    );
    expect([...second.checked!.keys()]).toEqual(['a', 'b']);

    const third = clickOutcome(
      ctrl,
      node('d'),
      ctx({ ...afterPlainClick('a'), checked: second.checked! }),
    );
    expect([...third.checked!.keys()]).toEqual(['a', 'b', 'd']);
  });

  it('counts the same first click for Ctrl as for Shift', () => {
    // The property, stated once: whichever modifier the operator reaches for second, the row they
    // clicked first is in the batch. Shift had it from 増分 1 and Ctrl did not, so one screen
    // marked three rows and moved two.
    const start = ctx(afterPlainClick('a'));
    expect(clickOutcome(ctrl, node('c'), start).checked!.has('a')).toBe(true);
    expect(clickOutcome(shift, node('c'), start).checked!.has('a')).toBe(true);
  });

  it('starts no batch from a folder the pane is showing', () => {
    const r = clickOutcome(ctrl, node('c'), ctx({ selection: { kind: 'group', id: 'g1' } }));
    expect([...r.checked!.keys()]).toEqual(['c']);
  });

  it('starts no batch from a row that is no longer on screen', () => {
    // Its folder was collapsed, a filter hid it, or the lazily-loaded page it came from was
    // replaced. Seeding a row the operator cannot see is the failure 決定 4 refuses for the anchor.
    const r = clickOutcome(ctrl, node('c'), ctx({ selection: { kind: 'node', id: 'gone' } }));
    expect([...r.checked!.keys()]).toEqual(['c']);
  });

  it('leaves a batch that already has members alone', () => {
    // Otherwise a row would come back the moment after it was Ctrl-clicked out of the batch, for
    // no reason the operator could see beyond the pane happening to show it.
    const r = clickOutcome(
      ctrl,
      node('d'),
      ctx({ selection: { kind: 'node', id: 'a' }, checked: checked('b') }),
    );
    expect([...r.checked!.keys()]).toEqual(['b', 'd']);
  });

  it('takes the pane row out again when it is the one Ctrl-clicked', () => {
    // Start the batch there, then toggle it — the answer a file manager gives for Ctrl-clicking
    // the row that is already selected.
    const r = clickOutcome(ctrl, node('a'), ctx(afterPlainClick('a')));
    expect([...r.checked!.keys()]).toEqual([]);
  });

  it('brings the pane row back on the next Ctrl click, deliberately', () => {
    // ⚠️ The one corner where this differs from a file manager, pinned so it reads as a decision
    // rather than an accident: plain-click a, Ctrl-click a to take it out, Ctrl-click b. Explorer
    // answers {b}; this answers {a, b}, because a still carries the pane's accent bar and the rule
    // is "the batch starts at the row the pane is showing". Avoiding it needs a "has the batch
    // been touched" flag — a state nothing else would read.
    const out = clickOutcome(ctrl, node('a'), ctx(afterPlainClick('a')));
    const back = clickOutcome(
      ctrl,
      node('b'),
      ctx({ ...afterPlainClick('a'), checked: out.checked! }),
    );
    expect([...back.checked!.keys()]).toEqual(['a', 'b']);
  });

  it('anchors a plain click even though it checks nothing', () => {
    const r = clickOutcome(plain, node('c'), ctx());
    expect([...r.checked!.keys()]).toEqual([]);
    expect(r.anchorId).toBe('c');
    expect(r.select).toBe(true);
  });

  it('abandons the batch on a plain click', () => {
    // "Never mind those" — the set goes, and the pane moves to the row that was clicked.
    const r = clickOutcome(plain, node('c'), ctx({ anchorId: 'a', checked: checked('a', 'b') }));
    expect([...r.checked!.keys()]).toEqual([]);
    expect(r.select).toBe(true);
  });

  it('leaves the pane alone for Ctrl and Shift', () => {
    // The pane keeps showing whatever was open while a batch is assembled — Ctrl / Shift never
    // write `?sel=`, which is what keeps ADR-073's three clear gestures untouched.
    expect(clickOutcome(ctrl, node('b'), ctx()).select).toBe(false);
    expect(clickOutcome(shift, node('b'), ctx({ anchorId: 'a' })).select).toBe(false);
  });

  it('moves the anchor to the row a Ctrl click landed on', () => {
    const r = clickOutcome(ctrl, node('c'), ctx({ anchorId: 'a' }));
    expect([...r.checked!.keys()]).toEqual(['c']);
    expect(r.anchorId).toBe('c');
  });

  it('writes nothing when a Shift click lands on a node that is not among the rows', () => {
    // `checked: null` means "leave the working set exactly as it is" — distinct from an empty
    // map, which would clear it. The anchor survives so the next Shift click still has a run.
    const r = clickOutcome(shift, node('elsewhere'), ctx({ anchorId: 'a', checked: checked('a') }));
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
