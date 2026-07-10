// Pure math for the corner drag-to-resize handle — no React, no DOM — so it's unit-testable in the
// node env. Converts a pointer pixel delta into a new {span, rowSpan}, each snapped to the widget's
// allowed steps. The grid is always treated as 12 columns (spans are stored 12-based; CSS auto-
// clamps the display at responsive breakpoints), so one column ≈ colPx+gapPx wide and one row ≈
// rowPx+gapPx tall — the +gap accounts for the track gap a span crosses when it grows by one.

import type { RowSpan, Span } from './types';

/** Snap a continuous value to the nearest entry in `allowed`. Ties go to the smaller value
 *  (strict `<` reducer, matching `clampSpan` in layout.ts). `allowed` must be non-empty. */
export function snapToAllowed<T extends number>(value: number, allowed: readonly T[]): T {
  return allowed.reduce((best, v) => (Math.abs(v - value) < Math.abs(best - value) ? v : best));
}

export interface SnapInput {
  /** Span/rowSpan at pointer-down. */
  startSpan: number;
  startRow: number;
  /** Pointer movement since down, in px. */
  dxPx: number;
  dyPx: number;
  /** One 1/12 column track width and one row-unit height, in px. */
  colPx: number;
  rowPx: number;
  /** Grid gap, in px. */
  gapPx: number;
  /** The steps this widget permits. `allowedRowSpans` empty ⇒ fixed height (locked to 1). */
  allowedSpans: readonly Span[];
  allowedRowSpans: readonly RowSpan[];
}

/** New {span, rowSpan} for a corner drag, each snapped to the widget's allowed steps. Width always
 *  snaps (every widget has ≥1 allowed span); height locks to 1 when the widget is fixed-height. */
export function snapSize(p: SnapInput): { span: Span; rowSpan: RowSpan } {
  const stepX = p.colPx + p.gapPx;
  const stepY = p.rowPx + p.gapPx;
  const targetSpan = stepX > 0 ? p.startSpan + p.dxPx / stepX : p.startSpan;
  const targetRow = stepY > 0 ? p.startRow + p.dyPx / stepY : p.startRow;
  return {
    span: snapToAllowed(targetSpan, p.allowedSpans),
    rowSpan: p.allowedRowSpans.length ? snapToAllowed(targetRow, p.allowedRowSpans) : (1 as RowSpan),
  };
}

/** Step to the neighbouring allowed value in `dir` (+1/−1) from `current` (for keyboard resize).
 *  Clamps at the ends. `allowed` need not be sorted. */
export function stepAllowed<T extends number>(current: T, dir: 1 | -1, allowed: readonly T[]): T {
  const sorted = [...allowed].sort((a, b) => a - b);
  const i = sorted.indexOf(current);
  if (i === -1) return snapToAllowed(current, sorted); // current not in set ⇒ snap in first
  const next = i + dir;
  return next >= 0 && next < sorted.length ? sorted[next] : current;
}
