// SPDX-License-Identifier: AGPL-3.0-only
// What a modified click does to the tree's **working set** — the nodes checked for a bulk action
// (ADR-124 決定 2). Separate from the primary selection in every way that matters:
//
//   - the primary selection drives the right-hand pane, lives in `?sel=` and is single;
//   - the working set drives the move controls, lives only in the page's state, and is a set.
//
// A plain click still drives the pane exactly as it did before this file existed, including
// ADR-073's "click the selected row again to clear it". Ctrl / Shift never touch `?sel=`, so the
// two cannot fight — which is the whole reason they are two things rather than one widened
// selection type. What a plain click *also* does, since 増分 1, is set the Shift anchor: it
// changes nothing the operator can see on its own, and without it a range can never start from
// an ordinary click (see `clickOutcome`).
//
// 🚨 **Two states, one selection — the operator reads the marked rows as one set, and they are
// right.** The pane's row carries an accent bar and the batch's rows carry a tint, so a plain
// click followed by Ctrl clicks paints every row involved. Both modified gestures therefore have
// to count that first click: Shift does it through the anchor (増分 1) and Ctrl through
// `batchStart` (増分 3). Before 増分 3 only Shift did, so the same screen showed three marked rows
// and moved two.
//
// ⚠️ **The working set holds whole nodes, not ids.** Filtering the tree changes which nodes are
// loaded, and a set of ids resolved against the current rows would silently shrink under the
// operator: they would check twelve nodes, type in the search box, and move nine.

import type { FlatRow } from '../../lib/nodeTree';
import type { NodeSummary } from '../../types/api';
// Type-only, so nothing here loads a `.tsx` — the same shape `lib/treeSelection.ts` imports.
import type { TreeSelection } from './NodeTree';

/** The checked nodes, keyed by id. Insertion order is the order they were checked. */
export type CheckedNodes = ReadonlyMap<string, NodeSummary>;

/** The node a flat row carries, or null for a row that is not a node (a folder, the Ungrouped
 *  header, a loading placeholder). Range selection walks over these. */
export function rowNode(row: FlatRow): NodeSummary | null {
  return row.kind === 'node' || row.kind === 'ungrouped-node' ? row.node : null;
}

/**
 * Whether a gesture on this row acts on the **working set** rather than on the row alone
 * (ADR-124 Inc.2, extracted in Inc.4).
 *
 * The file manager's rule, in one sentence: a row that is in the set carries the set. A set of just
 * this row is the row — there is nothing else in it to carry.
 *
 * 🚨 **Three gestures ask this and they must not each answer it.** The right-click menu and the
 * hover ↗ have read it since Inc.2 (through `nodeMoveItems`); the drag did not, and moved one node
 * when the operator had selected three — the same defect Inc.2 fixed, reappearing in the one
 * gesture that had its own copy of the decision. A fourth caller calls this; it does not spell
 * `checked.has(id) && checked.size > 1` again.
 *
 * ⚠️ **Takes `unknown` values on purpose.** Callers hold `CheckedNodes`, but nothing here reads a
 * node — widening the value type is what lets the menu module ask without importing this one's
 * vocabulary.
 */
export function actsOnSelection(checked: ReadonlyMap<string, unknown>, nodeId: string): boolean {
  return checked.has(nodeId) && checked.size > 1;
}

/** Ctrl / ⌘ click: add this node to the working set, or take it out if it is already in.
 *
 *  Returns a new map — the caller replaces its state with it, so React sees the change. */
export function toggleChecked(current: CheckedNodes, node: NodeSummary): Map<string, NodeSummary> {
  const next = new Map(current);
  if (!next.delete(node.id)) next.set(node.id, node);
  return next;
}

/** The result of a Shift click: the new working set and the anchor to remember. */
export interface RangeResult {
  checked: Map<string, NodeSummary>;
  /** The anchor for the *next* Shift click. Unchanged when the range worked; the clicked row when
   *  the old anchor had gone and this click had to start a new run. */
  anchorId: string;
}

/**
 * Shift click: add every node between the anchor row and the clicked row, both ends included.
 *
 * **Adds — never removes.** A second Shift click grows the set rather than replacing it, so the
 * rule is one sentence ("Ctrl picks one, Shift picks a run, and the set only grows") and no
 * gesture can silently drop nodes the operator had already collected. Clearing is a plain click,
 * Escape, or the bar's own button — three ways out, which is ADR-073's standard.
 *
 * 🚨 **The anchor is an id, never an index.** The flat row list is rebuilt whenever an SSE frame
 * lands, a filter changes, or a lazily-loaded folder arrives, so an index kept across two clicks
 * points at whatever row happens to sit there now. When the anchor's row is no longer on screen —
 * its folder was collapsed, a filter hid it, the search page it came from was replaced — this
 * click **becomes a Ctrl click** and starts a new run from where it landed. Silently ranging from
 * row 0 instead is how a click that meant "these two" selects a thousand.
 *
 * ⚠️ **Only rows that are on screen are in range.** Nodes inside a collapsed folder, and nodes a
 * lazily-loaded folder has not delivered, are not included and are not fetched to make them so:
 * "select what you can see" is a rule an operator can check by looking, and expanding folders on
 * their behalf would put nodes they never saw into a move.
 *
 * Returns null when the clicked row is not a node at all (a folder, a placeholder) — the caller
 * leaves the working set alone.
 */
export function rangeChecked(
  flat: readonly FlatRow[],
  anchorId: string | null,
  target: NodeSummary,
  current: CheckedNodes,
): RangeResult | null {
  const indexOfNode = (id: string) => flat.findIndex((row) => rowNode(row)?.id === id);
  const to = indexOfNode(target.id);
  if (to < 0) return null;
  // No anchor, or the anchor's row is gone: this click starts a new run from where it landed.
  const newRun = () => ({ checked: toggleChecked(current, target), anchorId: target.id });
  if (!anchorId) return newRun();
  const from = indexOfNode(anchorId);
  if (from < 0) return newRun();
  const [lo, hi] = from <= to ? [from, to] : [to, from];
  const next = new Map(current);
  for (let i = lo; i <= hi; i += 1) {
    const node = rowNode(flat[i]);
    if (node) next.set(node.id, node);
  }
  return { checked: next, anchorId };
}

/** Which gesture a click on a node row is, from the event's modifier keys.
 *
 *  `metaKey` is ⌘ on macOS, where Ctrl means something else entirely; both spellings map to the
 *  same gesture. Shift wins when both are held — that is what every file manager does. */
export function clickGesture(e: {
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
}): 'plain' | 'toggle' | 'range' {
  if (e.shiftKey) return 'range';
  return e.ctrlKey || e.metaKey ? 'toggle' : 'plain';
}

/**
 * Everything a click on a node row is decided from.
 *
 * One object rather than five positional arguments (増分 3): `anchorId` and the pane selection's
 * id are both nullable strings, and two adjacent positional parameters of the same type are a
 * swap that compiles, runs, and picks the wrong row to start a batch at.
 */
export interface ClickContext {
  /** The rows on screen, in display order — the flattened tree with collapse and filter applied. */
  flat: readonly FlatRow[];
  /** The row the next Shift click measures its range from. */
  anchorId: string | null;
  /** What the pane is showing, exactly as the tree holds it. Passed whole rather than as an id, so
   *  "a folder does not start a batch" is decided here and not in the component. */
  selection: TreeSelection;
  /** The working set as it stands. */
  checked: CheckedNodes;
}

/** The node behind a row id, when that row is on screen. Null for a folder, for a row inside a
 *  collapsed one, for a row a filter is hiding, and for one a lazily-loaded folder has not
 *  delivered — every case in which the operator cannot see it. */
function nodeOnScreen(flat: readonly FlatRow[], id: string): NodeSummary | null {
  for (const row of flat) {
    const node = rowNode(row);
    if (node?.id === id) return node;
  }
  return null;
}

/**
 * The set a Ctrl click adds to (ADR-124 増分 3).
 *
 * 🚨 **An empty working set starts at the row the pane is showing.** Without this, a plain click
 * followed by Ctrl clicks loses the first node: the plain click empties the batch, so the Ctrl
 * clicks start from nothing and only they are collected. Reported from the running box with a
 * screenshot of three marked rows, two of which would move.
 *
 * The rule is what Shift has done since 増分 1 — a plain click sets the anchor and `rangeChecked`
 * includes both ends — so this is the same first click being counted by the other modifier, not a
 * new idea. It is also what every file manager does.
 *
 * **The pane's row, not the anchor.** They are the same row immediately after a plain click, but a
 * Ctrl click moves the anchor and leaves the pane where it is. What the operator can see is the
 * pane's row, which carries the accent bar — so that is what may be seeded.
 *
 * ⚠️ **Only while the batch is empty.** Adding the pane's row to a batch that already has members
 * would bring a row back the moment after it was Ctrl-clicked out of the batch, for no reason the
 * operator could see other than that the pane happens to be showing it.
 */
function batchStart(ctx: ClickContext): CheckedNodes {
  if (ctx.checked.size > 0 || ctx.selection?.kind !== 'node') return ctx.checked;
  const node = nodeOnScreen(ctx.flat, ctx.selection.id);
  return node ? new Map([[node.id, node]]) : ctx.checked;
}

/** Everything a click on a node row decides. The component applies it and decides nothing. */
export interface ClickOutcome {
  /** The working set after the click, or null to leave it exactly as it is (no state write). */
  checked: Map<string, NodeSummary> | null;
  /** The row the *next* Shift click measures its range from. Read only when `checked` is set. */
  anchorId: string | null;
  /** Move the pane's own selection to this row — a plain click, and only a plain click. Carries
   *  ADR-073's clear-on-re-click with it, because the caller's `selectNode` is unchanged. */
  select: boolean;
}

/**
 * What a click on a node row does, once the modifier keys are read (ADR-124 決定 2/4 + 増分 1).
 *
 * 🚨 **A plain click sets the anchor.** ADR-124 shipped with "a plain click changes not one byte",
 * which left `anchorId` null until a Ctrl or Shift click had already happened — so the ordinary
 * two-click range (click a row, Shift-click a row further down) reached `rangeChecked` with no
 * anchor, fell to its new-run branch, and checked **only the row that was Shift-clicked**. The
 * intermediate rows were not missed; the range never started. This is the whole of the fix, and
 * it lives here rather than in the component because that is what makes it testable: the first
 * version's judgement sat in `NodeTree.tsx`, where Vitest cannot reach it, and the unit tests
 * covered `rangeChecked` with an anchor already supplied — never who supplies it.
 *
 * Every file manager anchors on a plain click. Nothing else in the tree reads `anchorId`, so
 * setting it costs one number and buys the gesture people already know.
 *
 * 🚨 **And the Ctrl click counts that same first click** (増分 3, `batchStart`). Shift had counted
 * it since 増分 1 and Ctrl had not, so one gesture kept the row the operator started from and the
 * other silently dropped it — while the tree painted both rows as marked.
 */
export function clickOutcome(
  e: { ctrlKey: boolean; metaKey: boolean; shiftKey: boolean },
  target: NodeSummary,
  ctx: ClickContext,
): ClickOutcome {
  switch (clickGesture(e)) {
    case 'toggle':
      return {
        checked: toggleChecked(batchStart(ctx), target),
        anchorId: target.id,
        select: false,
      };
    case 'range': {
      // Deliberately `ctx.checked`, never `batchStart`: the anchor already carries the first
      // click, and seeding the new-run branch (the anchor's row has gone) would quietly add a row
      // the range does not cover — the failure 決定 4 exists to refuse.
      const next = rangeChecked(ctx.flat, ctx.anchorId, target, ctx.checked);
      // Null ⇒ the clicked node is not among the visible rows at all. Leave the set alone rather
      // than guessing; the click still selects nothing, because Shift never drives the pane.
      return next
        ? { checked: next.checked, anchorId: next.anchorId, select: false }
        : { checked: null, anchorId: ctx.anchorId, select: false };
    }
    case 'plain':
      // Abandoning the batch is deliberate: a plain click means "never mind those". The anchor
      // moves here even though nothing is checked — that, and the pane selection this click is
      // about to write, are what the next Shift or Ctrl click starts the batch from.
      return { checked: new Map(), anchorId: target.id, select: true };
  }
}
