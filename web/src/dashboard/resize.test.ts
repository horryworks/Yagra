// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { snapSize, snapToAllowed, stepAllowed } from './resize';
import type { RowSpan, Span } from './types';

const SPANS: Span[] = [4, 6, 8, 12];
const ROWS: RowSpan[] = [1, 2, 3];

// One 1/12 column = 90px, one row unit = 240px, 18px gap ⇒ step of 108px horizontally, 258px
// vertically. So dragging +216px right ≈ +2 columns, +258px down ≈ +1 row.
const base = { colPx: 90, rowPx: 240, gapPx: 18, allowedSpans: SPANS, allowedRowSpans: ROWS };

describe('snapToAllowed', () => {
  it('picks the nearest allowed value, ties to the smaller', () => {
    expect(snapToAllowed(7, SPANS)).toBe(6); // 7 is equidistant 6/8 ⇒ smaller
    expect(snapToAllowed(7.5, SPANS)).toBe(8);
    expect(snapToAllowed(100, SPANS)).toBe(12);
    expect(snapToAllowed(-3, SPANS)).toBe(4);
  });
});

describe('snapSize', () => {
  it('snaps width by column steps and height by row steps', () => {
    // start 4×1, drag +216px right (~+2 cols) and +258px down (~+1 row) ⇒ 6×2 (4→ nearest of 6)
    const r = snapSize({ ...base, startSpan: 4, startRow: 1, dxPx: 216, dyPx: 258 });
    expect(r).toEqual({ span: 6, rowSpan: 2 });
  });

  it('grows to the max and shrinks to the min within the allowed set', () => {
    expect(snapSize({ ...base, startSpan: 8, startRow: 1, dxPx: 1000, dyPx: 1000 })).toEqual({
      span: 12,
      rowSpan: 3,
    });
    expect(snapSize({ ...base, startSpan: 8, startRow: 3, dxPx: -1000, dyPx: -1000 })).toEqual({
      span: 4,
      rowSpan: 1,
    });
  });

  it('locks height to 1 for a fixed-height widget (no allowed row spans)', () => {
    const r = snapSize({
      ...base,
      allowedRowSpans: [],
      startSpan: 6,
      startRow: 1,
      dxPx: 200,
      dyPx: 500,
    });
    expect(r).toEqual({ span: 8, rowSpan: 1 });
  });

  it('returns the start size when the grid could not be measured (zero track px)', () => {
    const r = snapSize({
      colPx: 0,
      rowPx: 0,
      gapPx: 0,
      allowedSpans: SPANS,
      allowedRowSpans: ROWS,
      startSpan: 6,
      startRow: 2,
      dxPx: 500,
      dyPx: 500,
    });
    expect(r).toEqual({ span: 6, rowSpan: 2 });
  });
});

describe('stepAllowed', () => {
  it('moves to the neighbouring allowed value and clamps at the ends', () => {
    expect(stepAllowed(6 as Span, 1, SPANS)).toBe(8);
    expect(stepAllowed(6 as Span, -1, SPANS)).toBe(4);
    expect(stepAllowed(12 as Span, 1, SPANS)).toBe(12); // clamp high
    expect(stepAllowed(4 as Span, -1, SPANS)).toBe(4); // clamp low
  });

  it('snaps a value not in the set before stepping', () => {
    expect(stepAllowed(7 as Span, 1, SPANS)).toBe(6); // 7 not in set ⇒ snap to 6
  });
});
