import { describe, expect, it } from 'vitest';
import { latestErrorRate, sparklinePath } from './interfaceMetrics';
import type { InterfaceSeries } from '../../types/api';

function series(partial: Partial<InterfaceSeries>): InterfaceSeries {
  return {
    timestamps: [],
    in_bps: [],
    out_bps: [],
    in_errors: [],
    out_errors: [],
    ...partial,
  };
}

describe('latestErrorRate', () => {
  it('returns null for an absent series', () => {
    expect(latestErrorRate(null)).toBeNull();
  });

  it('returns null when there are no error samples', () => {
    expect(latestErrorRate(series({ in_errors: [], out_errors: [] }))).toBeNull();
    expect(latestErrorRate(series({ in_errors: [null, null], out_errors: [null] }))).toBeNull();
  });

  it('sums the latest non-null in + out error rates', () => {
    expect(latestErrorRate(series({ in_errors: [1, 2, 3], out_errors: [0, 0, 4] }))).toBe(7);
  });

  it('skips trailing gaps to find the last real value per direction', () => {
    expect(
      latestErrorRate(series({ in_errors: [5, null, null], out_errors: [null, 2, null] })),
    ).toBe(7);
  });

  it('treats a one-sided absence as zero for that direction', () => {
    expect(latestErrorRate(series({ in_errors: [9], out_errors: [] }))).toBe(9);
    expect(latestErrorRate(series({ in_errors: [], out_errors: [3] }))).toBe(3);
  });
});

describe('sparklinePath', () => {
  it('returns null for fewer than two points (nothing to draw)', () => {
    expect(sparklinePath([], 120, 26)).toBeNull();
    expect(sparklinePath([5], 120, 26)).toBeNull();
  });

  it('builds a line that spans the padded box width and a closed area below it', () => {
    const p = sparklinePath([0, 10], 120, 26, 2)!;
    expect(p).not.toBeNull();
    // First point at the left inset, last point at width - inset.
    expect(p.line.startsWith('M2.0 ')).toBe(true);
    expect(p.line).toContain('L118.0 ');
    // The area closes down to the baseline (height - pad = 24) and back, then Z.
    expect(p.area).toContain('L118.0 24.0');
    expect(p.area).toContain('L2.0 24.0');
    expect(p.area.endsWith('Z')).toBe(true);
  });

  it('places the peak value at the top inset and the trough at the baseline', () => {
    // Two points: min (0) and max (10). With ×1.1 headroom the peak sits below the very top.
    const p = sparklinePath([0, 10], 100, 20, 2)!;
    // y for the min value (0) is the baseline: pad + innerH = 2 + 16 = 18.
    expect(p.line).toContain('M2.0 18.0');
    // y for the max value (10): 2 + 16 - (10 / 11) * 16 ≈ 3.5 — near the top inset.
    expect(p.line).toMatch(/L98\.0 3\.5/);
  });

  it('clamps negative values to the baseline rather than drawing below it', () => {
    const p = sparklinePath([-5, 10], 100, 20, 2)!;
    expect(p.line).toContain('M2.0 18.0'); // -5 clamped to the 0-baseline
  });
});
