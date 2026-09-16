// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's keyboard (ADR-155). Every movement case asserts where the cursor LANDS, not
// only that it moved — a walk that skipped one row too many passes "it moved" in every test.
import { describe, expect, it } from 'vitest';
import {
  anchorOnSettle,
  ariaLevel,
  cursorForMove,
  cursorAfterSelection,
  firstChildIndex,
  indexOfSelection,
  isStop,
  jumpIndex,
  keyBelongsToTree,
  menuStep,
  moveCheckedChange,
  moveGesture,
  pageRows,
  parentIndex,
  rowDomId,
  sameSelection,
  settleCursor,
  shouldRefocusTree,
  spaceCheckedChange,
  treeKeyAction,
  type KeyContext,
  type KeyPress,
} from './nodeTreeKeys';
import type { ClickContext } from './nodeTreeSelect';
import type { FlatRow } from '../../lib/nodeTree';
import type { NodeSummary } from '../../types/api';

const node = (id: string): NodeSummary => ({ id, name: id }) as NodeSummary;
const nodeRow = (id: string, depth = 1): FlatRow => ({ kind: 'node', depth, node: node(id) });
const ungroupedRow = (id: string): FlatRow => ({ kind: 'ungrouped-node', depth: 1, node: node(id) });
const groupRow = (id: string, depth = 0, isOpen = true, hasChildren = true): FlatRow =>
  ({ kind: 'group', depth, group: { id }, isOpen, hasChildren, tally: null }) as unknown as FlatRow;
const loadingRow = (depth = 1): FlatRow => ({ kind: 'group-loading', depth, groupId: 'gl' });
const failedRow = (depth = 1): FlatRow => ({ kind: 'group-failed', depth, groupId: 'gf' });
const headRow = (): FlatRow => ({ kind: 'ungrouped-head', count: 2 });

/**
 *   0  g:region          (open)
 *   1    g:site          (open)
 *   2      n:sw1
 *   3      n:sw2
 *   4    g:empty         (closed, no children)
 *   5    loading…
 *   6  g:closed          (closed)
 *   7  Ungrouped
 *   8    n:loose1
 *   9    n:loose2
 */
const TREE: FlatRow[] = [
  groupRow('region', 0),
  groupRow('site', 1),
  nodeRow('sw1', 2),
  nodeRow('sw2', 2),
  groupRow('empty', 1, false, false),
  loadingRow(1),
  groupRow('closed', 0, false, true),
  headRow(),
  ungroupedRow('loose1'),
  ungroupedRow('loose2'),
];

const key = (k: string, mods: Partial<KeyPress> = {}): KeyPress => ({
  key: k,
  shiftKey: false,
  ctrlKey: false,
  metaKey: false,
  altKey: false,
  ...mods,
});

const at = (cursor: number, flat: readonly FlatRow[] = TREE, page = 3): KeyContext => ({
  flat,
  cursor,
  page,
});

const checked = (...ids: string[]) => new Map(ids.map((id) => [id, node(id)]));

describe('rows the cursor rests on', () => {
  it('stops on folders and nodes, and walks over placeholders and the Ungrouped header', () => {
    expect(TREE.map(isStop)).toEqual([true, true, true, true, true, false, true, false, true, true]);
    expect(isStop(failedRow())).toBe(false);
    expect(isStop(undefined)).toBe(false);
  });

  it('finds a selection on screen, and answers -1 for one that is not', () => {
    expect(indexOfSelection(TREE, { kind: 'node', id: 'loose2' })).toBe(9);
    expect(indexOfSelection(TREE, { kind: 'group', id: 'site' })).toBe(1);
    // Same id, other kind: a folder and a node never stand for each other.
    expect(indexOfSelection(TREE, { kind: 'node', id: 'site' })).toBe(-1);
    expect(indexOfSelection(TREE, null)).toBe(-1);
  });

  it('gives a node one DOM id whichever section it is drawn in', () => {
    expect(rowDomId({ kind: 'node', id: 'x' })).toBe('ntree-n:x');
    expect(rowDomId({ kind: 'group', id: 'x' })).toBe('ntree-g:x');
  });

  it('compares selections by kind and id, and two empties are the same', () => {
    expect(sameSelection({ kind: 'node', id: 'a' }, { kind: 'node', id: 'a' })).toBe(true);
    expect(sameSelection({ kind: 'node', id: 'a' }, { kind: 'group', id: 'a' })).toBe(false);
    expect(sameSelection(null, null)).toBe(true);
    expect(sameSelection(null, { kind: 'node', id: 'a' })).toBe(false);
  });
});

describe('ariaLevel', () => {
  it('announces a row one below its depth, and an Ungrouped node at the top', () => {
    expect(ariaLevel(TREE[0])).toBe(1);
    expect(ariaLevel(TREE[2])).toBe(3);
    expect(ariaLevel(TREE[8])).toBe(1);
  });
});

describe('jumpIndex', () => {
  it('steps to the next and previous row', () => {
    expect(jumpIndex(TREE, 2, 1)).toBe(3);
    expect(jumpIndex(TREE, 3, -1)).toBe(2);
  });

  it('walks over a loading row and the Ungrouped header in both directions', () => {
    expect(jumpIndex(TREE, 4, 1)).toBe(6);
    expect(jumpIndex(TREE, 6, -1)).toBe(4);
    expect(jumpIndex(TREE, 6, 1)).toBe(8);
    expect(jumpIndex(TREE, 8, -1)).toBe(6);
  });

  it('stays at an end rather than wrapping', () => {
    expect(jumpIndex(TREE, 9, 1)).toBe(9);
    expect(jumpIndex(TREE, 0, -1)).toBe(0);
  });

  it('starts at the first row going down and the last going up when there is no cursor', () => {
    expect(jumpIndex(TREE, -1, 1)).toBe(0);
    expect(jumpIndex(TREE, -1, -1)).toBe(9);
  });

  it('pages by the given number of rows and lands on a row it may rest on', () => {
    // 2 + 3 = 5 is the loading row: the page keeps going to the next stop.
    expect(jumpIndex(TREE, 2, 3)).toBe(6);
    expect(jumpIndex(TREE, 8, -3)).toBe(4);
    // Past the end clamps to the last row.
    expect(jumpIndex(TREE, 6, 100)).toBe(9);
    expect(jumpIndex(TREE, 3, -100)).toBe(0);
  });

  it('comes back toward the start when a page runs off the end over rows it cannot rest on', () => {
    const flat = [nodeRow('a'), nodeRow('b'), loadingRow(), headRow()];
    expect(jumpIndex(flat, 0, 10)).toBe(1);
  });

  it('terminates when the cursor sits on a row it cannot rest on', () => {
    // The row changed kind under the cursor. Before the bounds on the backward scan this looped.
    const flat = [nodeRow('a'), loadingRow()];
    expect(jumpIndex(flat, 1, 1)).toBe(0);
  });

  it('answers -1 for a tree with nothing to rest on', () => {
    expect(jumpIndex([loadingRow(), headRow()], -1, 1)).toBe(-1);
  });
});

describe('pageRows', () => {
  it('is a screenful less one row, never under one', () => {
    expect(pageRows(390, 30)).toBe(12);
    expect(pageRows(29, 30)).toBe(1);
    expect(pageRows(0, 30)).toBe(1);
    expect(pageRows(300, 0)).toBe(1);
  });
});

describe('the hierarchy', () => {
  it('finds a row’s parent folder', () => {
    expect(parentIndex(TREE, 2)).toBe(1);
    expect(parentIndex(TREE, 3)).toBe(1);
    expect(parentIndex(TREE, 1)).toBe(0);
    expect(parentIndex(TREE, 4)).toBe(0);
  });

  it('gives a top-level folder and an ungrouped node no parent', () => {
    expect(parentIndex(TREE, 0)).toBe(-1);
    expect(parentIndex(TREE, 8)).toBe(-1);
    // The header is not a folder — even a folder above the header must not be read as the parent.
    expect(parentIndex([groupRow('g', 0), headRow(), nodeRow('stray', 1)], 2)).toBe(-1);
  });

  it('finds an open folder’s first child, and nothing for a closed or loading one', () => {
    expect(firstChildIndex(TREE, 0)).toBe(1);
    expect(firstChildIndex(TREE, 1)).toBe(2);
    expect(firstChildIndex(TREE, 6)).toBe(-1);
    expect(firstChildIndex([groupRow('g', 0), loadingRow(1)], 0)).toBe(-1);
    expect(firstChildIndex(TREE, 2)).toBe(-1);
  });
});

describe('treeKeyAction — movement', () => {
  it('moves down and up with a plain gesture', () => {
    expect(treeKeyAction(key('ArrowDown'), at(2))).toEqual({ kind: 'move', index: 3, gesture: 'plain' });
    expect(treeKeyAction(key('ArrowUp'), at(3))).toEqual({ kind: 'move', index: 2, gesture: 'plain' });
  });

  it('reads Shift as a range and Ctrl / ⌘ as leaving the working set alone', () => {
    expect(treeKeyAction(key('ArrowDown', { shiftKey: true }), at(2))).toMatchObject({ gesture: 'range' });
    expect(treeKeyAction(key('ArrowDown', { ctrlKey: true }), at(2))).toMatchObject({ gesture: 'keep' });
    expect(treeKeyAction(key('ArrowDown', { metaKey: true }), at(2))).toMatchObject({ gesture: 'keep' });
    expect(moveGesture({ shiftKey: true, ctrlKey: true, metaKey: false })).toBe('range');
  });

  // 🚨 A move onto the row the cursor already holds would re-apply the plain gesture and empty the
  // working set — a held Down at the bottom of the tree would throw the operator's batch away.
  it('claims a key that cannot move, without a move', () => {
    expect(treeKeyAction(key('ArrowDown'), at(9))).toEqual({ kind: 'none' });
    expect(treeKeyAction(key('Home'), at(0))).toEqual({ kind: 'none' });
  });

  it('goes to the ends with Home and End, and pages with PageUp and PageDown', () => {
    expect(treeKeyAction(key('End'), at(2))).toEqual({ kind: 'move', index: 9, gesture: 'plain' });
    expect(treeKeyAction(key('Home'), at(8))).toEqual({ kind: 'move', index: 0, gesture: 'plain' });
    expect(treeKeyAction(key('PageDown'), at(0, TREE, 2))).toEqual({ kind: 'move', index: 2, gesture: 'plain' });
    expect(treeKeyAction(key('PageUp'), at(9, TREE, 2))).toEqual({ kind: 'move', index: 6, gesture: 'plain' });
  });

  it('picks the first row with Down and the last with Up when nothing is selected', () => {
    expect(treeKeyAction(key('ArrowDown'), at(-1))).toEqual({ kind: 'move', index: 0, gesture: 'plain' });
    expect(treeKeyAction(key('ArrowUp'), at(-1))).toEqual({ kind: 'move', index: 9, gesture: 'plain' });
  });

  it('leaves Alt, Tab and Escape to the browser and the page', () => {
    expect(treeKeyAction(key('ArrowDown', { altKey: true }), at(2))).toBeNull();
    expect(treeKeyAction(key('Tab'), at(2))).toBeNull();
    // Escape is `escapeDismiss.ts`'s, decided once for the whole page (ADR-073).
    expect(treeKeyAction(key('Escape'), at(2))).toBeNull();
    expect(treeKeyAction(key('a'), at(2))).toBeNull();
  });
});

describe('treeKeyAction — folders', () => {
  it('opens a closed folder with Right, and steps into an open one', () => {
    expect(treeKeyAction(key('ArrowRight'), at(6))).toEqual({ kind: 'set-open', index: 6, open: true });
    expect(treeKeyAction(key('ArrowRight'), at(1))).toEqual({ kind: 'move', index: 2, gesture: 'plain' });
  });

  it('closes an open folder with Left, and steps out to the parent otherwise', () => {
    expect(treeKeyAction(key('ArrowLeft'), at(1))).toEqual({ kind: 'set-open', index: 1, open: false });
    expect(treeKeyAction(key('ArrowLeft'), at(3))).toEqual({ kind: 'move', index: 1, gesture: 'plain' });
    expect(treeKeyAction(key('ArrowLeft'), at(6))).toEqual({ kind: 'none' });
    // A folder with nothing in it is "open" by default, but has nothing to close: Left goes up.
    expect(treeKeyAction(key('ArrowLeft'), at(4))).toEqual({ kind: 'move', index: 0, gesture: 'plain' });
  });

  it('does not open a folder with nothing in it, and does nothing on a node', () => {
    expect(treeKeyAction(key('ArrowRight'), at(4))).toEqual({ kind: 'none' });
    expect(treeKeyAction(key('ArrowRight'), at(2))).toEqual({ kind: 'none' });
    expect(treeKeyAction(key('Enter'), at(4))).toEqual({ kind: 'none' });
    expect(treeKeyAction(key('ArrowLeft'), at(8))).toEqual({ kind: 'none' });
  });

  it('toggles a folder with Enter and opens a node’s page', () => {
    expect(treeKeyAction(key('Enter'), at(1))).toEqual({ kind: 'set-open', index: 1, open: false });
    expect(treeKeyAction(key('Enter'), at(6))).toEqual({ kind: 'set-open', index: 6, open: true });
    expect(treeKeyAction(key('Enter'), at(8))).toEqual({ kind: 'open-node', index: 8 });
  });

  it('leaves the modified spellings of Right, Left and Enter alone', () => {
    expect(treeKeyAction(key('ArrowRight', { shiftKey: true }), at(6))).toBeNull();
    expect(treeKeyAction(key('ArrowLeft', { ctrlKey: true }), at(1))).toBeNull();
    expect(treeKeyAction(key('Enter', { metaKey: true }), at(8))).toBeNull();
  });
});

describe('treeKeyAction — Space and the menu', () => {
  it('checks a node with Space or Ctrl+Space, and claims it on a folder', () => {
    expect(treeKeyAction(key(' '), at(2))).toEqual({ kind: 'check', index: 2 });
    expect(treeKeyAction(key(' ', { ctrlKey: true }), at(2))).toEqual({ kind: 'check', index: 2 });
    // Claimed rather than let through: Space on a focused scroller scrolls it.
    expect(treeKeyAction(key(' '), at(0))).toEqual({ kind: 'none' });
    expect(treeKeyAction(key(' ', { shiftKey: true }), at(2))).toBeNull();
  });

  it('opens the menu with the context-menu key and with Shift+F10', () => {
    expect(treeKeyAction(key('ContextMenu'), at(2))).toEqual({ kind: 'menu', index: 2 });
    expect(treeKeyAction(key('F10', { shiftKey: true }), at(0))).toEqual({ kind: 'menu', index: 0 });
    expect(treeKeyAction(key('F10'), at(0))).toBeNull();
  });

  // Tab into the tree selects nothing; only a movement key picks the first row.
  it('does nothing but claim the key when there is no current row', () => {
    for (const k of ['ArrowRight', 'ArrowLeft', 'Enter', ' ', 'ContextMenu']) {
      expect(treeKeyAction(key(k), at(-1)), k).toEqual({ kind: 'none' });
    }
  });
});

describe('moveCheckedChange', () => {
  const ctx = (over: Partial<ClickContext> = {}): ClickContext => ({
    flat: TREE,
    anchorId: null,
    selection: { kind: 'node', id: 'sw1' },
    checked: new Map(),
    ...over,
  });

  // 🚨 THE HELD-KEY CASE. Writing an empty set over an empty set on every repeat re-renders the
  // whole page — detail pane and all — thirty times a second.
  it('writes nothing for a plain move over an empty working set', () => {
    expect(moveCheckedChange('plain', node('sw2'), ctx())).toBeNull();
  });

  it('abandons a working set on a plain move, as a plain click does', () => {
    const out = moveCheckedChange('plain', node('sw2'), ctx({ checked: checked('loose1', 'loose2') }));
    expect(out && [...out.checked.keys()]).toEqual([]);
    expect(out?.anchorId).toBe('sw2');
  });

  it('leaves the working set alone on a Ctrl move', () => {
    expect(moveCheckedChange('keep', node('sw2'), ctx({ checked: checked('loose1') }))).toBeNull();
  });

  it('starts a Shift range at the row the cursor leaves when the set is empty', () => {
    // The stored anchor is stale (`loose2`, far below) — an empty set has no run to extend.
    const out = moveCheckedChange('range', node('sw2'), ctx({ anchorId: 'loose2' }));
    expect(out && [...out.checked.keys()].sort()).toEqual(['sw1', 'sw2']);
  });

  it('extends a Shift range from the anchor when a set exists, over the folders between', () => {
    const out = moveCheckedChange(
      'range',
      node('loose1'),
      ctx({ anchorId: 'sw2', checked: checked('sw2'), selection: { kind: 'group', id: 'closed' } }),
    );
    expect(out && [...out.checked.keys()].sort()).toEqual(['loose1', 'sw2']);
    expect(out?.anchorId).toBe('sw2');
  });
});

describe('spaceCheckedChange', () => {
  // 🚨 THE FIRST SPACE. Through `clickOutcome`'s Ctrl branch an empty set is seeded with the pane's
  // row — which, from the keyboard, is the row Space was pressed on — and then toggled straight out.
  it('puts the current row in on the first press, with the pane showing that same row', () => {
    const out = spaceCheckedChange(node('sw1'), new Map());
    expect([...out.checked.keys()]).toEqual(['sw1']);
    expect(out.anchorId).toBe('sw1');
  });

  it('takes a row that is in the set out again', () => {
    expect([...spaceCheckedChange(node('sw1'), checked('sw1', 'sw2')).checked.keys()]).toEqual(['sw2']);
  });
});

describe('anchorOnSettle', () => {
  it('moves the anchor to where the keys rested while the set is empty', () => {
    expect(anchorOnSettle(0, 'sw1', 'loose1')).toBe('loose1');
    expect(anchorOnSettle(0, null, 'loose1')).toBe('loose1');
  });

  it('writes nothing when it is already there or a set exists', () => {
    expect(anchorOnSettle(0, 'loose1', 'loose1')).toBeNull();
    expect(anchorOnSettle(2, 'sw1', 'loose1')).toBeNull();
  });
});

describe('settleCursor', () => {
  const a = { kind: 'node', id: 'a' } as const;
  const b = { kind: 'node', id: 'b' } as const;

  it('commits a cursor that has rested and differs from the selection', () => {
    expect(settleCursor(b, b, a, null, true)).toBe('commit');
  });

  it('waits while a newer press is pending', () => {
    const newer = { kind: 'node', id: 'c' } as const;
    expect(settleCursor(b, newer, a, null, true)).toBe('wait');
    expect(settleCursor(null, null, a, null, true)).toBe('wait');
  });

  // 🚨 THE REPEATED WRITE. The effect re-runs on every render (the page hands new callbacks each
  // time), and between the write and the URL catching up it would write again, and again.
  it('does not write the same press twice', () => {
    expect(settleCursor(b, b, a, b, true)).toBe('wait');
    // A new press back onto the same row is a new object, and is written.
    const again = { ...b };
    expect(settleCursor(again, again, a, b, true)).toBe('commit');
  });

  it('drops a cursor the selection already agrees with, or whose row has gone', () => {
    expect(settleCursor(b, b, { kind: 'node', id: 'b' }, null, true)).toBe('clear');
    expect(settleCursor(b, b, a, null, false)).toBe('clear');
  });
});

describe('cursorForMove', () => {
  const a = { kind: 'node', id: 'a' } as const;
  const b = { kind: 'node', id: 'b' } as const;

  it('keeps a move away from the selection, and drops one back onto it', () => {
    expect(cursorForMove(b, a, null)).toBe(b);
    expect(cursorForMove(a, a, null)).toBeNull();
  });

  // 🚨 A→B was written and the URL has not landed; ↑ back to A must not be dropped, or the URL lands
  // on B and the tree shows B although the operator's last press was A.
  it('keeps a move back onto the selection while a write is on its way', () => {
    expect(cursorForMove(a, a, b)).toBe(a);
  });
});

describe('cursorAfterSelection', () => {
  const a = { kind: 'node', id: 'a' } as const;
  const b = { kind: 'node', id: 'b' } as const;
  const c = { kind: 'node', id: 'c' } as const;

  it('drops the cursor once its own write lands', () => {
    expect(cursorAfterSelection(b, b, b)).toBeNull();
  });

  // 🚨 THE LOST PRESS. The URL lands a frame after the write; a key pressed in that frame is newer
  // than the write and must not be thrown away with it.
  it('keeps a press made after the write it is catching up with', () => {
    expect(cursorAfterSelection(c, b, b)).toBe(c);
  });

  it('drops the cursor when something else moved the selection', () => {
    // Escape cleared it while a press was pending.
    expect(cursorAfterSelection(b, null, null)).toBeNull();
    expect(cursorAfterSelection(b, a, b)).toBeNull();
  });

  it('has nothing to do without a cursor', () => {
    expect(cursorAfterSelection(null, a, a)).toBeNull();
  });
});

describe('menuStep', () => {
  it('wraps Up and Down, and goes to the ends with Home and End', () => {
    expect(menuStep('ArrowDown', 2, 3)).toBe(0);
    expect(menuStep('ArrowUp', 0, 3)).toBe(2);
    expect(menuStep('ArrowDown', 0, 3)).toBe(1);
    expect(menuStep('Home', 2, 3)).toBe(0);
    expect(menuStep('End', 0, 3)).toBe(2);
  });

  it('enters from nowhere at the first or last item', () => {
    expect(menuStep('ArrowDown', -1, 3)).toBe(0);
    expect(menuStep('ArrowUp', -1, 3)).toBe(2);
  });

  it('answers nothing for other keys and for an empty menu', () => {
    expect(menuStep('Enter', 0, 3)).toBeNull();
    expect(menuStep('ArrowDown', 0, 0)).toBeNull();
  });
});

describe('shouldRefocusTree', () => {
  it('takes focus back when the menu took it away with it', () => {
    expect(shouldRefocusTree(true, false)).toBe(true);
  });

  it('leaves focus where an item put it, and never pulls it out from under a dialog', () => {
    expect(shouldRefocusTree(false, false)).toBe(false);
    expect(shouldRefocusTree(true, true)).toBe(false);
  });
});

describe('keyBelongsToTree', () => {
  const body = { tabIndex: 0 };

  it('answers the body itself and a control taken out of the Tab order (the row’s name button)', () => {
    expect(keyBelongsToTree(body, body, true)).toBe(true);
    expect(keyBelongsToTree({ tabIndex: -1 }, body, true)).toBe(true);
  });

  it('leaves a control that is in the Tab order its own keys', () => {
    // Enter on Retry retries; it does not open a folder.
    expect(keyBelongsToTree({ tabIndex: 0 }, body, true)).toBe(false);
  });

  // 🚨 React bubbles through portals: the row's ＋ menu is not inside the tree in the DOM, and its
  // items are `tabIndex = -1`, so without this check Enter on one would be taken by the tree.
  it('refuses a key that came through a portal', () => {
    expect(keyBelongsToTree({ tabIndex: -1 }, body, false)).toBe(false);
    expect(keyBelongsToTree(null, body, true)).toBe(false);
  });
});
