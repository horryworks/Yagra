// SPDX-License-Identifier: AGPL-3.0-only
// Where a popover goes, given what it is anchored to, how big it measured, and the viewport.
//
// Two anchors, two functions, deliberately not one. A popover opened from a *trigger* hangs off the
// trigger's edge — below it, aligned to one side — and flips above when it does not fit. A context
// menu opened at a *point* has no edge: it opens down-and-right from the pointer, the way every
// native menu does, and when that does not fit it opens up or left instead. Folding the two into
// one function over a zero-size rect would give the point the trigger's GAP and its side alignment,
// neither of which a pointer has.
//
// **Why a `.ts`.** Until ADR-124 Inc.2 the trigger arithmetic lived inside `AnchoredPopover`'s
// `useCallback`, where Vitest never ran it; the ten broken screens ADR-088 Inc.3 found were
// placement bugs of exactly the kind a unit test shows. Both functions here are pure over numbers.

export interface Size {
  width: number;
  height: number;
}

export interface Viewport {
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

/** The subset of `DOMRect` the trigger placement reads. */
export interface TriggerRect {
  top: number;
  bottom: number;
  left: number;
  right: number;
}

export interface Placement {
  top: number;
  left: number;
  /** Set only when the popover is taller than the viewport allows: it is pinned to the top edge and
   *  told how tall it may be, and the surface scrolls the rest. Absent, no height is imposed. */
  maxHeight?: number;
}

/** Whether a re-measure landed where the popover already is — the case a caller skips the state
 *  write for, so a ResizeObserver or a scroll cannot re-render a popover that did not move. */
export function samePlacement(a: Placement | null, b: Placement): boolean {
  return a !== null && a.top === b.top && a.left === b.left && a.maxHeight === b.maxHeight;
}

/** Gap between a trigger and its popover. */
export const GAP = 4;
/** The minimum distance a popover keeps from every viewport edge. */
export const EDGE = 8;

/**
 * From a trigger: below it, aligned to the chosen edge, clamped inside the viewport; above it when
 * below does not fit. Verbatim what `AnchoredPopover` did before this file existed — every caller
 * that opens from a button depends on this not changing.
 */
export function placeFromTrigger(
  t: TriggerRect,
  m: Size,
  vp: Viewport,
  align: 'start' | 'end',
): Placement {
  const maxLeft = Math.max(EDGE, vp.width - m.width - EDGE);
  const left = Math.min(Math.max(align === 'start' ? t.left : t.right - m.width, EDGE), maxLeft);
  let top = t.bottom + GAP;
  if (top + m.height > vp.height - EDGE) top = Math.max(EDGE, t.top - m.height - GAP);
  return { top, left };
}

/**
 * One axis of the point placement. `p` is the pointer, `size` the popover's extent along this axis,
 * `extent` the viewport's. In order: after the point; before it (flipped, so the popover ends where
 * the pointer is); shifted back until it fits; and, when it cannot fit at all, pinned to the leading
 * edge with the room there is.
 */
function along(p: number, size: number, extent: number): { start: number; max?: number } {
  const room = extent - 2 * EDGE;
  if (size > room) return { start: EDGE, max: room };
  const from = Math.max(EDGE, p);
  if (from + size <= extent - EDGE) return { start: from };
  if (p - size >= EDGE) return { start: p - size };
  return { start: extent - size - EDGE };
}

/**
 * From a point (a right-click): down-and-right of it, as a native context menu opens; up and/or
 * left when that does not fit; shifted into view when neither does; and scrolling inside a
 * `maxHeight` when it is taller than the viewport.
 */
export function placeFromPoint(p: Point, m: Size, vp: Viewport): Placement {
  const v = along(p.y, m.height, vp.height);
  const h = along(p.x, m.width, vp.width);
  return v.max === undefined
    ? { top: v.start, left: h.start }
    : { top: v.start, left: h.start, maxHeight: v.max };
}
