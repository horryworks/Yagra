// SPDX-License-Identifier: AGPL-3.0-only
// The topology map's viewport transform. `layout.ts` decides where nodes go; this decides what the
// operator sees of them, and its degenerate cases (empty topology, unmeasured pane) are the ones
// that render a blank map with nothing in the console.

import { describe, expect, it } from 'vitest';
import { clampScale, fitView, MAX_SCALE, MIN_SCALE } from './fitView';
import type { GraphLayout } from './graphLayout';

const layout = (width: number, height: number): GraphLayout =>
  ({ width, height, nodes: [], edges: [] }) as unknown as GraphLayout;

describe('fitView', () => {
  it('centers the diagram in the viewport', () => {
    // A 100×100 diagram in a 1000×1000 pane: scale is capped, and the leftover space is split.
    const v = fitView(layout(100, 100), 1000, 1000);
    expect(v.scale).toBe(MAX_SCALE);
    expect(v.tx).toBeCloseTo((1000 - 100 * MAX_SCALE) / 2);
    expect(v.ty).toBeCloseTo((1000 - 100 * MAX_SCALE) / 2);
  });

  it('fits to whichever axis is tighter, leaving a margin', () => {
    // Wide diagram, square pane ⇒ width is the constraint.
    const v = fitView(layout(1000, 100), 500, 500);
    expect(v.scale).toBeCloseTo((500 / 1000) * 0.92);
  });

  it('never zooms past the bounds a gesture is also clamped to', () => {
    // A huge diagram would otherwise fit at an unreadable scale.
    expect(fitView(layout(100_000, 100_000), 500, 500).scale).toBe(MIN_SCALE);
    // A tiny one would otherwise blow up to fill the pane.
    expect(fitView(layout(1, 1), 5000, 5000).scale).toBe(MAX_SCALE);
  });

  it('returns the identity transform when there is nothing to fit', () => {
    // Each of these reaches a division by zero without the guard, and a NaN transform renders an
    // empty map with no error anywhere.
    const identity = { tx: 0, ty: 0, scale: 1 };
    expect(fitView(layout(0, 100), 500, 500)).toEqual(identity);
    expect(fitView(layout(100, 0), 500, 500)).toEqual(identity);
    expect(fitView(layout(100, 100), 0, 500)).toEqual(identity);
    expect(fitView(layout(100, 100), 500, 0)).toEqual(identity);
  });

  it('produces a finite transform for every non-degenerate input', () => {
    for (const [w, h, vw, vh] of [
      [800, 600, 1024, 768],
      [50, 4000, 300, 300],
      [4000, 50, 300, 300],
    ]) {
      const v = fitView(layout(w, h), vw, vh);
      expect(Number.isFinite(v.tx)).toBe(true);
      expect(Number.isFinite(v.ty)).toBe(true);
      expect(Number.isFinite(v.scale)).toBe(true);
    }
  });
});

describe('clampScale', () => {
  it('holds a pinch or wheel gesture inside the fit', () => {
    expect(clampScale(0.01)).toBe(MIN_SCALE);
    expect(clampScale(100)).toBe(MAX_SCALE);
    expect(clampScale(1)).toBe(1);
  });
});
