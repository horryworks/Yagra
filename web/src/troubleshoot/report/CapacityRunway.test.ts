// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { MAX_HORIZON_DAYS, NEAR_TERM_DAYS, buildRunway } from './CapacityRunway';

describe('buildRunway', () => {
  it('places the crossing where the trend reaches 100%', () => {
    // 50% now, +1 pp/day ⇒ 50 days to full. Horizon = 50*1.2 = 60 days, so the crossing sits at 5/6
    // of the plot width.
    const m = buildRunway({ current: 50, slope_per_day: 1, tte_days: 50 })!;
    expect(m.horizonDays).toBeCloseTo(60);
    expect(m.cross).not.toBeNull();
    const frac = (m.cross!.x - 2) / (m.w - 4);
    expect(frac).toBeCloseTo(50 / 60, 2);
    // The crossing is on the ceiling line by definition.
    expect(m.cross!.y).toBeCloseTo(m.ceilingY);
  });

  it('never divides by a flat or falling slope', () => {
    // The backend only emits rising projections, but a defensive body must not produce Infinity/NaN
    // geometry if it ever sees one.
    for (const slope of [0, -0.5]) {
      const m = buildRunway({ current: 40, slope_per_day: slope, tte_days: 0 })!;
      expect(m.cross).toBeNull();
      expect(m.line).not.toMatch(/NaN|Infinity/);
      expect(m.ceilingY).toBeGreaterThan(0);
    }
  });

  it('treats an already-full resource as crossing at day zero', () => {
    const m = buildRunway({ current: 100, slope_per_day: 0.2, tte_days: 1 })!;
    expect(m.cross).not.toBeNull();
    // x = the left edge (PADX), i.e. the very start of the plot.
    expect(m.cross!.x).toBeCloseTo(2);
  });

  it('clamps the horizon at both ends so short and long runways stay readable', () => {
    // A 2-day runway would otherwise render as a vertical cliff.
    expect(buildRunway({ current: 90, slope_per_day: 5, tte_days: 2 })!.horizonDays).toBe(
      NEAR_TERM_DAYS,
    );
    // A multi-year projection is clamped to the drawable year.
    expect(buildRunway({ current: 1, slope_per_day: 0.01, tte_days: 9900 })!.horizonDays).toBe(
      MAX_HORIZON_DAYS,
    );
  });

  it('rejects non-finite input rather than emitting broken geometry', () => {
    expect(buildRunway({ current: NaN, slope_per_day: 1, tte_days: 10 })).toBeNull();
    expect(buildRunway({ current: 10, slope_per_day: Infinity, tte_days: 10 })).toBeNull();
  });

  it('keeps the near-term band inside the plot', () => {
    const m = buildRunway({ current: 10, slope_per_day: 0.1, tte_days: 900 })!;
    expect(m.nearW).toBeGreaterThan(0);
    expect(m.nearW).toBeLessThanOrEqual(m.w - 4);
  });
});
