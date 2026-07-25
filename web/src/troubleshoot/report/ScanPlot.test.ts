// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { buildScanPlot, type ScanPoint } from './ScanPlot';

const p = (src: string, dst: number, ports: number): ScanPoint => ({
  src,
  distinctDst: dst,
  distinctPorts: ports,
  severity: 'info',
});

describe('buildScanPlot', () => {
  it('returns null with nothing to plot', () => {
    expect(buildScanPlot([])).toBeNull();
  });

  it('never produces -Infinity from a zero or one count', () => {
    // log10(0) is -Infinity; the floor must map it to the axis origin instead.
    const m = buildScanPlot([p('a', 0, 0), p('b', 1, 1)])!;
    for (const pt of m.points) {
      expect(Number.isFinite(pt.x)).toBe(true);
      expect(Number.isFinite(pt.y)).toBe(true);
    }
    // Both sit at the origin corner.
    expect(m.points[0].x).toBeCloseTo(m.points[1].x);
    expect(m.points[0].y).toBeCloseTo(m.points[1].y);
  });

  it('separates a horizontal sweep from a vertical probe across the diagonal', () => {
    // The whole point of the plot: these two must land on opposite sides.
    const m = buildScanPlot([p('sweep', 4496, 20), p('probe', 20, 85)])!;
    const [sweep, probe] = m.points;
    // A sweep is far right and low; a probe is left and high (y grows upward = smaller y value).
    expect(sweep.x).toBeGreaterThan(probe.x);
    expect(sweep.y).toBeGreaterThan(probe.y);
  });

  it('shares one decade bound on both axes so the diagonal is a true 45° line', () => {
    const m = buildScanPlot([p('a', 900, 3)])!;
    // Ticks are whole decades and identical in count on both axes.
    expect(m.xTicks.map((t) => t.v)).toEqual(m.yTicks.map((t) => t.v));
    expect(m.xTicks.map((t) => t.v)).toEqual([1, 10, 100, 1000]);
  });

  it('keeps every point inside the plot rect', () => {
    const m = buildScanPlot([p('a', 1, 1), p('b', 5000, 5000), p('c', 70, 900)])!;
    for (const pt of m.points) {
      expect(pt.x).toBeGreaterThanOrEqual(m.plot.x - 0.001);
      expect(pt.x).toBeLessThanOrEqual(m.plot.x + m.plot.w + 0.001);
      expect(pt.y).toBeGreaterThanOrEqual(m.plot.y - 0.001);
      expect(pt.y).toBeLessThanOrEqual(m.plot.y + m.plot.h + 0.001);
    }
  });

  it('always spans at least one decade, even when every count is tiny', () => {
    const m = buildScanPlot([p('a', 2, 3)])!;
    expect(m.xTicks.length).toBeGreaterThanOrEqual(2);
    expect(m.plot.w).toBeGreaterThan(0);
  });
});
