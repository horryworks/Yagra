// SPDX-License-Identifier: AGPL-3.0-only
// What a key pressed in the inventory tree does (ADR-155).
//
// The tree is one focus stop: `.ntree-body` takes focus and says which row is current through
// `aria-activedescendant`. The rows are never focused themselves — they are virtualized, and a row
// that scrolls out of the window takes a focus it held with it, so a tree whose rows roved focus
// stopped answering the arrows after a turn of the wheel.
//
// 🚨 **The cursor IS the pane's selection.** Down moves the selection and the detail pane follows,
// which is what was asked for. There is no second "focused but not selected" mark to keep apart
// from `.sel` — the one extra state is the short window in which a key has moved the selection and
// the URL has not been written yet (`settleCursor`).
//
// ⚠️ **Every decision is here, and the component applies them.** Vitest never loads a `.tsx`
// (`.claude/rules/testing.md`), and ADR-124 paid twice for judgement left beside a handler. No DOM
// type appears in any signature: a test hands over plain objects.

import type { FlatRow } from '../../lib/nodeTree';
import type { NodeSummary } from '../../types/api';
// Type-only, so nothing here loads a `.tsx` — the same shape `nodeTreeSelect.ts` uses.
import type { TreeSelection } from './NodeTree';
import {
  clickGesture,
  clickOutcome,
  rowNode,
  toggleChecked,
  type CheckedNodes,
  type ClickContext,
} from './nodeTreeSelect';

/**
 * How long the keys must rest before a moved cursor is written to `?sel=` (ADR-155 決定 3).
 *
 * 🚨 **Not a nicety — two things break without it.** `?sel=` is written with `history.replaceState`,
 * which Safari refuses past 100 calls in 30 seconds with a `SecurityError`, and a held arrow key
 * repeats about 30 times a second; this app has no error boundary, so that is a blank page
 * (`lib/useUrlTerm.ts` met the same limit in the search box). And the detail pane is keyed on the
 * node, so every write rebuilds it and starts its three fetches.
 *
 * Shorter than `SEARCH_DEBOUNCE_MS` (200) on purpose: that one waits for a person to finish a word,
 * this one only for a key to stop repeating, and the pane is what the operator is waiting to read.
 */
export const CURSOR_SETTLE_MS = 100;

/** The modifier keys a decision reads — a `KeyboardEvent` satisfies this structurally. */
export interface KeyPress {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
}

/** Whether two selections name the same row. Two nulls are the same (nothing selected). */
export function sameSelection(a: TreeSelection, b: TreeSelection): boolean {
  return (a?.kind ?? null) === (b?.kind ?? null) && (a?.id ?? null) === (b?.id ?? null);
}

/** The selection a row stands for, or null for a row the cursor cannot rest on. */
export function rowSelection(row: FlatRow | undefined): TreeSelection {
  if (!row) return null;
  if (row.kind === 'group') return { kind: 'group', id: row.group.id };
  const node = rowNode(row);
  return node ? { kind: 'node', id: node.id } : null;
}

/** Whether the cursor may rest on a row: a folder or a node. A loading placeholder, a failed
 *  fetch and the Ungrouped header are walked over — none of them is something to select. */
export function isStop(row: FlatRow | undefined): boolean {
  return rowSelection(row) !== null;
}

/** Where a selection sits in the rows on screen, or -1 when it is not among them (a folder closed
 *  above it, a filter hiding it, or nothing selected). */
export function indexOfSelection(flat: readonly FlatRow[], sel: TreeSelection): number {
  if (!sel) return -1;
  return flat.findIndex((row) => sameSelection(rowSelection(row), sel));
}

/**
 * The DOM id a row carries, which `aria-activedescendant` names.
 *
 * Built from the selection rather than from `flatRowKey`, so the two spellings of a node row
 * (`node` and `ungrouped-node`) cannot give one node two ids. The colon is harmless: nothing looks
 * the id up with a CSS selector.
 */
export function rowDomId(sel: NonNullable<TreeSelection>): string {
  return `ntree-${sel.kind === 'node' ? 'n' : 'g'}:${sel.id}`;
}

/** The `aria-level` a row announces. A folder or node is one level below its own depth's parent;
 *  an Ungrouped node is top-level, because the header above it is a label, not a folder. */
export function ariaLevel(row: FlatRow): number {
  switch (row.kind) {
    case 'ungrouped-node':
    case 'ungrouped-head':
      return 1;
    case 'group':
    case 'node':
    case 'group-loading':
    case 'group-failed':
      return row.depth + 1;
  }
}

function firstStop(flat: readonly FlatRow[]): number {
  return flat.findIndex((row) => isStop(row));
}

function lastStop(flat: readonly FlatRow[]): number {
  for (let i = flat.length - 1; i >= 0; i -= 1) if (isStop(flat[i])) return i;
  return -1;
}

/**
 * The row a jump of `delta` rows lands on.
 *
 * - From nowhere (`from < 0`) forward lands on the first row, backward on the last — what Down and
 *   Up do when nothing is selected.
 * - A jump that lands on a row the cursor cannot rest on keeps going the same way, and when that
 *   runs off the end it comes back toward where it started, never past it.
 * - At an end it stays put. It never wraps: in a tree of thousands of rows a wrap is a teleport.
 */
export function jumpIndex(flat: readonly FlatRow[], from: number, delta: number): number {
  if (from < 0 || from >= flat.length) return delta >= 0 ? firstStop(flat) : lastStop(flat);
  if (delta === 0) return from;
  const dir = delta > 0 ? 1 : -1;
  const target = Math.min(Math.max(from + delta, 0), flat.length - 1);
  for (let i = target; i >= 0 && i < flat.length; i += dir) if (isStop(flat[i])) return i;
  // Bounded on both sides as well as by `from`: a `from` the cursor cannot rest on (a row that
  // changed kind under it) would otherwise walk past it and off the array for ever.
  for (let i = target - dir; i !== from && i >= 0 && i < flat.length; i -= dir) {
    if (isStop(flat[i])) return i;
  }
  return from;
}

/** How many rows PageUp / PageDown move: one screenful less a row, so the row the operator was
 *  reading at the edge is still on screen after the jump. Never less than one. */
export function pageRows(clientHeight: number, rowHeight: number): number {
  if (rowHeight <= 0) return 1;
  return Math.max(1, Math.floor(clientHeight / rowHeight) - 1);
}

function depthOf(row: FlatRow | undefined): number {
  return row && 'depth' in row ? row.depth : -1;
}

/** The folder row directly above this row in the hierarchy, or -1. A node in Ungrouped has none —
 *  the header above it is not a folder. */
export function parentIndex(flat: readonly FlatRow[], index: number): number {
  const row = flat[index];
  if (!row || row.kind === 'ungrouped-node' || row.kind === 'ungrouped-head') return -1;
  const want = depthOf(row) - 1;
  if (want < 0) return -1;
  for (let i = index - 1; i >= 0; i -= 1) {
    const above = flat[i];
    if (above.kind === 'group' && above.depth === want) return i;
    if (above.kind === 'ungrouped-head') return -1;
  }
  return -1;
}

/** An open folder's first child row, when the cursor may rest on it. A folder whose first row is
 *  still loading answers -1: Right then does nothing rather than jumping past the placeholder. */
export function firstChildIndex(flat: readonly FlatRow[], index: number): number {
  const row = flat[index];
  if (row?.kind !== 'group' || !row.isOpen) return -1;
  const next = flat[index + 1];
  return next && depthOf(next) === row.depth + 1 && isStop(next) ? index + 1 : -1;
}

/** How a key that moves the cursor treats the working set — the click gesture it corresponds to
 *  (ADR-155 決定 5). `keep` is Ctrl / ⌘: move without touching the set. */
export type MoveGesture = 'plain' | 'range' | 'keep';

/** The same modifier reading a click gets (`clickGesture`), with Ctrl read as "leave the set
 *  alone" — a Ctrl-arrow has no row to toggle, so it moves and does nothing else. */
export function moveGesture(e: Pick<KeyPress, 'shiftKey' | 'ctrlKey' | 'metaKey'>): MoveGesture {
  const g = clickGesture(e);
  return g === 'toggle' ? 'keep' : g;
}

/** What a key press does, decided; the component applies it. */
export type TreeKeyOutcome =
  /** Put the cursor on this row. */
  | { kind: 'move'; index: number; gesture: MoveGesture }
  /** Open (`open: true`) or close this folder — the same press the ▶ makes. */
  | { kind: 'set-open'; index: number; open: boolean }
  /** Add this node to the working set, or take it out. */
  | { kind: 'check'; index: number }
  /** Open this node's own page. */
  | { kind: 'open-node'; index: number }
  /** Open this row's context menu from the keyboard. */
  | { kind: 'menu'; index: number }
  /** A key the tree answers that has nothing to do right now (Down at the last row). Still
   *  claimed: letting it through would scroll the pane, which is the browser's default for it. */
  | { kind: 'none' };

export interface KeyContext {
  /** The rows on screen, in order. */
  flat: readonly FlatRow[];
  /** Where the cursor is in `flat`, or -1. */
  cursor: number;
  /** Rows a PageUp / PageDown moves (`pageRows`). */
  page: number;
}

const NONE: TreeKeyOutcome = { kind: 'none' };

/** A move, unless it lands where the cursor already is — that is `none`, so a held Down at the last
 *  row does not re-apply a plain gesture and empty a working set the operator is looking at. */
function moveTo(index: number, ctx: KeyContext, gesture: MoveGesture): TreeKeyOutcome {
  return index < 0 || index === ctx.cursor ? NONE : { kind: 'move', index, gesture };
}

/**
 * What a key press in the tree does, or null for a key the tree does not answer (it is left to the
 * browser and to the page — Escape among them, see ADR-155 決定 8).
 *
 * The tree keys are WAI-ARIA's: Up/Down move, Right opens a folder or steps into it, Left closes one
 * or steps out to its parent, Home/End go to the ends, Enter opens, and the context-menu key or
 * Shift+F10 opens the row's menu. Space and the modified arrows are the working set's — read the
 * way the clicks are (`moveGesture`).
 *
 * ⚠️ **Nothing is chosen from nowhere except by the movement keys.** Right, Left, Enter, Space and
 * the menu key with no current row are claimed and do nothing: Tab into the tree selects nothing,
 * and the first Down is what picks a row.
 */
export function treeKeyAction(e: KeyPress, ctx: KeyContext): TreeKeyOutcome | null {
  // Alt+arrow is the browser's history navigation, and nothing in the tree is spelled with Alt.
  if (e.altKey) return null;
  const { flat, cursor } = ctx;
  const row = cursor >= 0 ? flat[cursor] : undefined;
  const modified = e.shiftKey || e.ctrlKey || e.metaKey;
  switch (e.key) {
    case 'ArrowDown':
      return moveTo(jumpIndex(flat, cursor, 1), ctx, moveGesture(e));
    case 'ArrowUp':
      return moveTo(jumpIndex(flat, cursor, -1), ctx, moveGesture(e));
    case 'PageDown':
      return moveTo(jumpIndex(flat, cursor, ctx.page), ctx, moveGesture(e));
    case 'PageUp':
      return moveTo(jumpIndex(flat, cursor, -ctx.page), ctx, moveGesture(e));
    case 'Home':
      return moveTo(firstStop(flat), ctx, moveGesture(e));
    case 'End':
      return moveTo(lastStop(flat), ctx, moveGesture(e));
    case 'ArrowRight': {
      if (modified) return null;
      if (row?.kind !== 'group') return NONE;
      if (!row.hasChildren) return NONE;
      if (!row.isOpen) return { kind: 'set-open', index: cursor, open: true };
      return moveTo(firstChildIndex(flat, cursor), ctx, 'plain');
    }
    case 'ArrowLeft': {
      if (modified) return null;
      if (!row) return NONE;
      if (row.kind === 'group' && row.isOpen && row.hasChildren) {
        return { kind: 'set-open', index: cursor, open: false };
      }
      return moveTo(parentIndex(flat, cursor), ctx, 'plain');
    }
    case 'Enter': {
      if (modified) return null;
      if (row?.kind === 'group') {
        return row.hasChildren ? { kind: 'set-open', index: cursor, open: !row.isOpen } : NONE;
      }
      return row && rowNode(row) ? { kind: 'open-node', index: cursor } : NONE;
    }
    case ' ':
      // Ctrl+Space is the file manager's spelling of the same toggle; both are accepted.
      if (e.shiftKey) return null;
      return row && rowNode(row) ? { kind: 'check', index: cursor } : NONE;
    case 'ContextMenu':
      return row ? { kind: 'menu', index: cursor } : NONE;
    case 'F10':
      if (!e.shiftKey || e.ctrlKey || e.metaKey) return null;
      return row ? { kind: 'menu', index: cursor } : NONE;
    default:
      return null;
  }
}

/** A working-set change to apply: the new set, and the row the next Shift gesture measures from. */
export interface CheckedChange {
  checked: Map<string, NodeSummary>;
  anchorId: string | null;
}

/**
 * What moving the cursor onto a node does to the working set (ADR-155 決定 5).
 *
 * Read through `clickOutcome`, so the keyboard and the mouse have one rule between them — with two
 * differences, both about the cursor being the pane's selection:
 *
 * 🚨 **A plain move over an empty set writes nothing.** A plain click empties the set and moves the
 * anchor, and doing that on every repeat of a held key re-renders the whole page 30 times a second
 * — the detail pane included — to write an empty set over an empty set. The anchor catches up once,
 * when the keys rest (`anchorOnSettle`).
 *
 * ⚠️ **An empty set's Shift range starts at the cursor.** With the anchor no longer following every
 * plain move, the stored anchor can be a row the operator left long ago. A set that is empty has no
 * run to extend, so the run starts where the operator is — which is also what a plain click would
 * have left as the anchor.
 */
export function moveCheckedChange(
  gesture: MoveGesture,
  target: NodeSummary,
  ctx: ClickContext,
): CheckedChange | null {
  switch (gesture) {
    case 'keep':
      return null;
    case 'plain': {
      if (ctx.checked.size === 0) return null;
      const o = clickOutcome({ ctrlKey: false, metaKey: false, shiftKey: false }, target, ctx);
      return o.checked ? { checked: o.checked, anchorId: o.anchorId } : null;
    }
    case 'range': {
      const from = ctx.selection?.kind === 'node' ? ctx.selection.id : null;
      const anchorId = ctx.checked.size === 0 ? (from ?? ctx.anchorId) : ctx.anchorId;
      const o = clickOutcome({ ctrlKey: false, metaKey: false, shiftKey: true }, target, {
        ...ctx,
        anchorId,
      });
      return o.checked ? { checked: o.checked, anchorId: o.anchorId } : null;
    }
  }
}

/**
 * What Space does: this node in or out of the working set, and the anchor moves to it.
 *
 * 🚨 **Not `clickOutcome`'s Ctrl branch.** That one seeds an empty set with the row the pane is
 * showing (`batchStart`, ADR-124 増分 3) — and from the keyboard the pane's row IS the row Space was
 * pressed on, so the seed would put it in and the toggle take it straight out: the first Space would
 * do nothing at all.
 */
export function spaceCheckedChange(target: NodeSummary, checked: CheckedNodes): CheckedChange {
  return { checked: toggleChecked(checked, target), anchorId: target.id };
}

/** The anchor to write once the keys rest on a node, or null to write nothing. Only while the set is
 *  empty: a set that exists keeps the anchor its own gestures gave it. */
export function anchorOnSettle(
  checkedSize: number,
  anchorId: string | null,
  nodeId: string,
): string | null {
  return checkedSize === 0 && anchorId !== nodeId ? nodeId : null;
}

/**
 * What to do once the cursor has stopped moving (ADR-155 決定 3).
 *
 * - `wait`: nothing yet — the debounced value is stale (a newer press is pending), or this exact
 *   press was already written and the URL has not caught up. **Identity, not equality**: a new press
 *   back onto the same row is a new object, and the one already written is the same object.
 * - `clear`: drop the cursor without writing — the selection already says the same thing, or the row
 *   is no longer on screen (a filter hid it while the key rested), so there is nothing to select.
 * - `commit`: write it.
 */
export function settleCursor(
  settled: TreeSelection,
  cursor: TreeSelection,
  selected: TreeSelection,
  committed: TreeSelection,
  onScreen: boolean,
): 'wait' | 'clear' | 'commit' {
  if (!settled || settled !== cursor || settled === committed) return 'wait';
  if (sameSelection(settled, selected) || !onScreen) return 'clear';
  return 'commit';
}

/**
 * The cursor a key move leaves: none when the move lands on what `?sel=` already says, so there is
 * nothing to write — **unless a write is still on its way**. Then `?sel=` is about to become that
 * write, and a press back onto the old row has to survive it or the tree lands on the wrong row.
 */
export function cursorForMove(
  sel: TreeSelection,
  selected: TreeSelection,
  committed: TreeSelection,
): TreeSelection {
  return sameSelection(sel, selected) && !committed ? null : sel;
}

/**
 * The cursor after `?sel=` changed (ADR-155 決定 3).
 *
 * 🚨 **The cursor is dropped when the URL catches up, never when it is written.** react-router
 * renders a location change as a transition, so the URL lands a frame or more after the write; a
 * cursor cleared at write time shows the previous row selected for that frame.
 *
 * - The change is the write this cursor made (`committed` now matches): drop the cursor — unless a
 *   key was pressed again after the write, which is newer than it and survives.
 * - Anything else moved the selection (Escape, a click on blank space, a link): the cursor is
 *   stale, and keeping it would put the selection back a moment later.
 */
export function cursorAfterSelection(
  cursor: TreeSelection,
  selected: TreeSelection,
  committed: TreeSelection,
): TreeSelection {
  if (!cursor) return null;
  if (committed && sameSelection(committed, selected)) {
    return sameSelection(cursor, selected) ? null : cursor;
  }
  return null;
}

/** The index a menu key moves to among `length` items (ADR-155 決定 7), or null for a key the menu
 *  does not answer. Up and Down wrap — a menu is short, and `ActionMenu` wraps too. */
export function menuStep(key: string, current: number, length: number): number | null {
  if (length <= 0) return null;
  switch (key) {
    case 'ArrowDown':
      return current < 0 ? 0 : (current + 1) % length;
    case 'ArrowUp':
      return current < 0 ? length - 1 : (current - 1 + length) % length;
    case 'Home':
      return 0;
    case 'End':
      return length - 1;
    default:
      return null;
  }
}

/**
 * Whether focus goes back to the tree once a menu the keyboard opened has closed (ADR-155 決定 7).
 *
 * The item that held focus is gone with the menu, which leaves focus on the document and the tree
 * one Tab away from where the operator was. But not over a dialog the item just opened (Edit node…):
 * `Modal` puts focus in itself, and taking it back would leave a dialog nobody can type into.
 */
export function shouldRefocusTree(focusIsOnDocument: boolean, dialogOpen: boolean): boolean {
  return focusIsOnDocument && !dialogOpen;
}

/** A key event's target and the tree body, as far as `keyBelongsToTree` reads them. */
export interface KeyTarget {
  tabIndex: number;
}

/**
 * Whether a key press that reached the tree body's handler is the tree's to answer.
 *
 * - 🚨 **It must have come from inside the body in the DOM.** React bubbles events through portals,
 *   so a key pressed in a row's `＋` menu — portalled to `document.body` — reaches this handler too,
 *   and answering Enter there would cancel the menu item it was meant for.
 * - A control that is itself in the Tab order (the retry button, a suppression marker, a hover
 *   action) keeps its own keys: Enter on Retry retries. The row's name buttons are out of the Tab
 *   order (`tabIndex = -1`), so a press on one — where focus sits after a click — is the tree's.
 */
export function keyBelongsToTree(
  target: KeyTarget | null,
  body: KeyTarget,
  containedInBody: boolean,
): boolean {
  if (!target || !containedInBody) return false;
  return target === body || target.tabIndex < 0;
}
