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
// ⚠️ **The working set holds whole nodes, not ids.** Filtering the tree changes which nodes are
// loaded, and a set of ids resolved against the current rows would silently shrink under the
// operator: they would check twelve nodes, type in the search box, and move nine.

import type { FlatRow } from '../../lib/nodeTree';
import type { NodeSummary } from '../../types/api';

/** The checked nodes, keyed by id. Insertion order is the order they were checked. */
export type CheckedNodes = ReadonlyMap<string, NodeSummary>;

/** The node a flat row carries, or null for a row that is not a node (a folder, the Ungrouped
 *  header, a loading placeholder). Range selection walks over these. */
export function rowNode(row: FlatRow): NodeSummary | null {
  return row.kind === 'node' || row.kind === 'ungrouped-node' ? row.node : null;
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
 */
export function clickOutcome(
  e: { ctrlKey: boolean; metaKey: boolean; shiftKey: boolean },
  flat: readonly FlatRow[],
  anchorId: string | null,
  target: NodeSummary,
  current: CheckedNodes,
): ClickOutcome {
  switch (clickGesture(e)) {
    case 'toggle':
      return { checked: toggleChecked(current, target), anchorId: target.id, select: false };
    case 'range': {
      const next = rangeChecked(flat, anchorId, target, current);
      // Null ⇒ the clicked node is not among the visible rows at all. Leave the set alone rather
      // than guessing; the click still selects nothing, because Shift never drives the pane.
      return next
        ? { checked: next.checked, anchorId: next.anchorId, select: false }
        : { checked: null, anchorId, select: false };
    }
    case 'plain':
      // Abandoning the batch is deliberate: a plain click means "never mind those". The anchor
      // moves here even though nothing is checked — that is the state a Shift click reads next.
      return { checked: new Map(), anchorId: target.id, select: true };
  }
}
