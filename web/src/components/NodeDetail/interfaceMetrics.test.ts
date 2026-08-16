// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  FAULT_SERIES,
  faultValues,
  hasOpticalData,
  latestDiscardRate,
  latestErrorRate,
  latestRxPower,
  latestTxPower,
  OPTICAL_SERIES,
  opticalValues,
  opticalYRange,
  sparklinePath,
  throughputBandwidthOverlay,
  throughputPair,
} from './interfaceMetrics';
import { formatBps } from '../../lib/format';
import type { InterfaceSeries } from '../../types/api';

function series(partial: Partial<InterfaceSeries>): InterfaceSeries {
  return {
    timestamps: [],
    in_bps: [],
    out_bps: [],
    in_ucast_pps: [],
    out_ucast_pps: [],
    in_errors: [],
    out_errors: [],
    in_discards: [],
    out_discards: [],
    rx_power_dbm: [],
    tx_power_dbm: [],
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

describe('latestDiscardRate', () => {
  it('returns null for an absent series', () => {
    expect(latestDiscardRate(null)).toBeNull();
  });

  it('returns null when there are no discard samples', () => {
    expect(latestDiscardRate(series({ in_discards: [], out_discards: [] }))).toBeNull();
    expect(
      latestDiscardRate(series({ in_discards: [null, null], out_discards: [null] })),
    ).toBeNull();
  });

  it('sums the latest non-null in + out discard rates, skipping trailing gaps', () => {
    expect(latestDiscardRate(series({ in_discards: [1, 2, 3], out_discards: [0, 0, 4] }))).toBe(7);
    expect(
      latestDiscardRate(series({ in_discards: [5, null, null], out_discards: [null, 2, null] })),
    ).toBe(7);
  });

  // The two helpers read four same-typed arrays off one object, so a transposed pair would be a
  // silent wrong answer rather than a type error. Feed each pair only its own values and assert
  // the other reads nothing.
  it('reads the discard arrays and not the error arrays', () => {
    const onlyDiscards = series({ in_discards: [11], out_discards: [22] });
    expect(latestDiscardRate(onlyDiscards)).toBe(33);
    expect(latestErrorRate(onlyDiscards)).toBeNull();

    const onlyErrors = series({ in_errors: [4], out_errors: [5] });
    expect(latestErrorRate(onlyErrors)).toBe(9);
    expect(latestDiscardRate(onlyErrors)).toBeNull();
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

describe('throughputBandwidthOverlay', () => {
  it('yields no line or range for an absent / non-positive speed', () => {
    expect(throughputBandwidthOverlay(null, 'fit', 'bps')).toEqual({});
    expect(throughputBandwidthOverlay(undefined, 'capacity', 'bps')).toEqual({});
    expect(throughputBandwidthOverlay(0, 'fit', 'bps')).toEqual({});
    expect(throughputBandwidthOverlay(-1, 'capacity', 'bps')).toEqual({});
  });

  it('draws the bandwidth line but leaves the axis auto-fit in fit mode', () => {
    const o = throughputBandwidthOverlay(1_000_000_000, 'fit', 'bps');
    expect(o.referenceLine).toEqual({ value: 1_000_000_000, label: formatBps(1_000_000_000) });
    expect(o.yRange).toBeUndefined();
  });

  it('pins the axis top to the bandwidth in capacity mode', () => {
    const o = throughputBandwidthOverlay(1_000_000_000, 'capacity', 'bps');
    expect(o.referenceLine?.value).toBe(1_000_000_000);
    expect(o.yRange).toEqual([0, 1_000_000_000]);
  });

  // ADR-060. `ifSpeed` is bits/sec, so on a packets/sec axis the line would sit at an arbitrary
  // height and read as a capacity — a wrong answer is worse than no answer. Both modes are
  // asserted because `capacity` additionally pins the Y range, and leaking that would squash the
  // whole packet-rate series into the bottom pixel of the chart.
  it('yields nothing in pps mode even when the interface has a bandwidth', () => {
    expect(throughputBandwidthOverlay(1_000_000_000, 'fit', 'pps')).toEqual({});
    expect(throughputBandwidthOverlay(1_000_000_000, 'capacity', 'pps')).toEqual({});
  });
});

describe('throughputPair', () => {
  const s = series({
    in_bps: [1000],
    out_bps: [2000],
    in_ucast_pps: [3],
    out_ucast_pps: [4],
  });

  it('reads the bps arrays in bps mode', () => {
    expect(throughputPair(s, 'bps')).toEqual([[1000], [2000]]);
  });

  it('reads the pps arrays in pps mode', () => {
    expect(throughputPair(s, 'pps')).toEqual([[3], [4]]);
  });

  // The four arrays are the same type, so a transposed pick compiles and mislabels the axis:
  // bit rates drawn under a `pps` heading, three orders of magnitude too high, with nothing to
  // show it is wrong. Assert each mode reads *only* its own pair.
  it('never mixes the two units', () => {
    const onlyBps = series({ in_bps: [1000], out_bps: [2000] });
    expect(throughputPair(onlyBps, 'pps')).toEqual([[], []]);

    const onlyPps = series({ in_ucast_pps: [3], out_ucast_pps: [4] });
    expect(throughputPair(onlyPps, 'bps')).toEqual([[], []]);
  });
});

describe('FAULT_SERIES', () => {
  // The chart merged two charts into one (ADR-046 Inc.5), so the four arrays are no longer split
  // across two components that could not confuse them. Pin the set: dropping one silently loses a
  // fault channel from the only place it is charted, and the chart still renders.
  it('plots exactly the four fault arrays, errors before discards', () => {
    expect(FAULT_SERIES.map((s) => s.key)).toEqual([
      'in_errors',
      'out_errors',
      'in_discards',
      'out_discards',
    ]);
  });

  // On a healthy link all four lines sit flat at zero and overlap, so hue is the ONLY thing telling
  // them apart once one of them rises. Two lines sharing a palette slot would be indistinguishable
  // exactly when the chart matters.
  it('gives every line its own palette slot', () => {
    const slots = FAULT_SERIES.map((s) => s.colorIndex);
    expect(new Set(slots).size).toBe(slots.length);
  });

  it('gives every line its own label key', () => {
    const keys = FAULT_SERIES.map((s) => s.labelKey);
    expect(new Set(keys).size).toBe(keys.length);
  });
});

describe('faultValues', () => {
  // Same hazard as `throughputPair`: four same-typed arrays on one object, so a transposed read is
  // a wrong line under a right label. Feed one array at a time and assert the other three see none.
  it('reads only the array its spec names', () => {
    const only = series({ out_discards: [7] });
    for (const spec of FAULT_SERIES) {
      expect(faultValues(only, spec)).toEqual(spec.key === 'out_discards' ? [7] : []);
    }
  });

  it('yields an empty array when the core did not send the field', () => {
    const partial = { timestamps: [1] } as unknown as InterfaceSeries;
    for (const spec of FAULT_SERIES) {
      expect(faultValues(partial, spec)).toEqual([]);
    }
  });
});

// ── Optical power (ADR-062) ──────────────────────────────────────────────────────────

describe('hasOpticalData', () => {
  // This predicate IS the "show the chart when the port is optical" rule, so its edges are the
  // feature's edges rather than a detail of it.
  it('is false before the fetch settles, and that must not be read as "not optical"', () => {
    expect(hasOpticalData(null)).toBe(false);
  });

  it('is false for a copper port, whose optical arrays are empty or all gaps', () => {
    expect(hasOpticalData(series({}))).toBe(false);
    expect(hasOpticalData(series({ rx_power_dbm: [null, null], tx_power_dbm: [null] }))).toBe(false);
  });

  it('is true when either direction has ever reported', () => {
    expect(hasOpticalData(series({ rx_power_dbm: [null, -7.4] }))).toBe(true);
    expect(hasOpticalData(series({ tx_power_dbm: [-2.1] }))).toBe(true);
  });

  // 🚨 The one that a truthiness test would get wrong. 0 dBm is one milliwatt — a strong signal,
  // and the exact value a healthy short-reach link sits near. `.some(Boolean)` would call this
  // port copper and hide its chart.
  it('is true for a reading of exactly 0 dBm', () => {
    expect(hasOpticalData(series({ rx_power_dbm: [0] }))).toBe(true);
  });

  it('is false when the core predates the optical fields entirely', () => {
    const partial = { timestamps: [1] } as unknown as InterfaceSeries;
    expect(hasOpticalData(partial)).toBe(false);
  });
});

describe('opticalValues', () => {
  // Same transposition hazard as `faultValues`, and worse in consequence: rx and tx are the same
  // type, the same magnitude and both plausible, so a swap produces a chart nobody can see is wrong.
  it('reads only the array its spec names', () => {
    const only = series({ tx_power_dbm: [-2.1] });
    for (const spec of OPTICAL_SERIES) {
      expect(opticalValues(only, spec)).toEqual(spec.key === 'tx_power_dbm' ? [-2.1] : []);
    }
  });

  it('yields an empty array when the core did not send the field', () => {
    const partial = { timestamps: [1] } as unknown as InterfaceSeries;
    for (const spec of OPTICAL_SERIES) {
      expect(opticalValues(partial, spec)).toEqual([]);
    }
  });

  it('gives every line its own palette slot and label key', () => {
    expect(new Set(OPTICAL_SERIES.map((s) => s.colorIndex)).size).toBe(OPTICAL_SERIES.length);
    expect(new Set(OPTICAL_SERIES.map((s) => s.labelKey)).size).toBe(OPTICAL_SERIES.length);
  });
});

describe('opticalYRange', () => {
  it('is undefined when there is nothing to bound', () => {
    expect(opticalYRange(null)).toBeUndefined();
    expect(opticalYRange(series({}))).toBeUndefined();
    expect(opticalYRange(series({ rx_power_dbm: [null] }))).toBeUndefined();
  });

  // The point of bounding at all: an axis that included 0 would squash the band a real link lives
  // in. Both ends must come from the data, not from zero.
  it('brackets the data across both directions with a dB of headroom', () => {
    const r = opticalYRange(series({ rx_power_dbm: [-7.4, -8.2], tx_power_dbm: [-2.1] }));
    expect(r).toEqual([-10, -1]);
  });

  it('never returns a zero-height range for a dead-flat series', () => {
    const r = opticalYRange(series({ rx_power_dbm: [-7, -7, -7] }));
    expect(r).toBeDefined();
    expect(r![1]).toBeGreaterThan(r![0]);
  });

  it('ignores gaps and non-finite values rather than collapsing on them', () => {
    const r = opticalYRange(series({ rx_power_dbm: [null, -7.4, Number.NaN] }));
    expect(r).toEqual([-9, -6]);
  });
});

describe('latestRxPower / latestTxPower', () => {
  it('are null for an absent series and for a port that reports nothing', () => {
    expect(latestRxPower(null)).toBeNull();
    expect(latestTxPower(null)).toBeNull();
    expect(latestRxPower(series({ rx_power_dbm: [null, null] }))).toBeNull();
  });

  it('read the last non-null sample of their own direction', () => {
    const s = series({ rx_power_dbm: [-7.4, null], tx_power_dbm: [-2.1, -2.3] });
    expect(latestRxPower(s)).toBe(-7.4);
    expect(latestTxPower(s)).toBe(-2.3);
  });

  // Not summed, unlike the error and discard tiles: adding two logarithms yields no physical
  // quantity, so the two tiles stay independent.
  it('keep the two directions independent', () => {
    expect(latestRxPower(series({ tx_power_dbm: [-2.1] }))).toBeNull();
    expect(latestTxPower(series({ rx_power_dbm: [-7.4] }))).toBeNull();
  });

  it('report a genuine 0 dBm rather than treating it as absent', () => {
    expect(latestRxPower(series({ rx_power_dbm: [0] }))).toBe(0);
  });
});
