// SPDX-License-Identifier: AGPL-3.0-only
// The Overview tab's arithmetic: pairing a memory source's two input ranges, and the KB gauge
// formatter.
//
// The bug class for the series is not "the chart looks wrong" — it is "the chart shows a percentage
// nobody measured". Total and free arrive as two independent reads, so anything other than an exact
// timestamp join derives a number from samples that never coexisted. These pin the join, and pin
// that a point with no partner (or no derivable percentage) becomes a gap rather than a zero.

import { describe, expect, it } from 'vitest';
import { formatKb, memPctSeries } from './overviewMetrics';
import type { ResolvedMem } from './metricCards';
import type { MetricPoint } from '../../types/api';

/** Huawei's pair: total + free bytes, so pct = (total - free) / total. */
const huawei: ResolvedMem = {
  id: 'huawei',
  metrics: ['huawei_mem_total', 'huawei_mem_free'],
  unitToBytes: 1,
};
/** UCD's pair reads in KB, so the same numbers must produce the same percentage. */
const ucd: ResolvedMem = {
  id: 'ucd',
  metrics: ['ucd_mem_total_kb', 'ucd_mem_avail_kb'],
  unitToBytes: 1024,
};

const pts = (...pairs: [number, number][]): MetricPoint[] => pairs.map(([t, v]) => ({ t, v }));

describe('memPctSeries', () => {
  it('derives a percentage per shared timestamp, in the first series order', () => {
    expect(
      memPctSeries(huawei, {
        huawei_mem_total: pts([1, 100], [2, 100], [3, 200]),
        huawei_mem_free: pts([1, 25], [2, 50], [3, 50]),
      }),
    ).toEqual({ timestamps: [1, 2, 3], values: [75, 50, 75] });
  });

  it('scales both inputs together, so the percentage is unit-independent', () => {
    // `unitToBytes` moves used and total by the same factor; a KB source must not read differently
    // from a byte source with the same readings.
    expect(
      memPctSeries(ucd, {
        ucd_mem_total_kb: pts([1, 100]),
        ucd_mem_avail_kb: pts([1, 25]),
      }).values,
    ).toEqual([75]);
  });

  it('yields nothing when the two series share no timestamp', () => {
    // Two reads that never coexisted. Pairing them by position (or by nearest) would draw a
    // percentage the device never reported.
    expect(
      memPctSeries(huawei, {
        huawei_mem_total: pts([1, 100], [2, 100]),
        huawei_mem_free: pts([3, 25], [4, 25]),
      }),
    ).toEqual({ timestamps: [], values: [] });
  });

  it('keeps only the overlap when one series is longer than the other', () => {
    expect(
      memPctSeries(huawei, {
        huawei_mem_total: pts([1, 100], [2, 100], [3, 100], [4, 100]),
        huawei_mem_free: pts([2, 10]),
      }),
    ).toEqual({ timestamps: [2], values: [90] });
    // ...and symmetrically, when it is the *second* series that has the extra points.
    expect(
      memPctSeries(huawei, {
        huawei_mem_total: pts([2, 100]),
        huawei_mem_free: pts([1, 10], [2, 10], [3, 10]),
      }),
    ).toEqual({ timestamps: [2], values: [90] });
  });

  it('treats a missing series as no data rather than throwing', () => {
    // A range fetch that failed leaves the metric out of the map entirely; the card must render an
    // empty chart, not crash the whole Overview.
    expect(memPctSeries(huawei, {})).toEqual({ timestamps: [], values: [] });
    expect(memPctSeries(huawei, { huawei_mem_total: pts([1, 100]) })).toEqual({
      timestamps: [],
      values: [],
    });
    expect(memPctSeries(huawei, { huawei_mem_free: pts([1, 25]) })).toEqual({
      timestamps: [],
      values: [],
    });
  });

  it('drops a point whose value is not a usable number', () => {
    // The API types `v` as a number, so this is the defensive half: a null/undefined slipping
    // through must become a gap, never a 0% (which reads as "the device freed all its memory").
    const holes = [null, undefined, NaN, Infinity];
    for (const bad of holes) {
      const series = memPctSeries(huawei, {
        huawei_mem_total: pts([1, 100], [2, 100]),
        huawei_mem_free: [{ t: 1, v: bad as unknown as number }, ...pts([2, 20])],
      });
      expect(series).toEqual({ timestamps: [2], values: [80] });
    }
  });

  it('drops a point with no derivable percentage', () => {
    // A zero (or absent) total divides to Infinity/NaN, which uPlot happily draws.
    expect(
      memPctSeries(huawei, {
        huawei_mem_total: pts([1, 0], [2, 100]),
        huawei_mem_free: pts([1, 0], [2, 40]),
      }),
    ).toEqual({ timestamps: [2], values: [60] });
  });

  it('yields nothing for empty input series', () => {
    expect(memPctSeries(huawei, { huawei_mem_total: [], huawei_mem_free: [] })).toEqual({
      timestamps: [],
      values: [],
    });
  });
});

describe('formatKb', () => {
  it('steps at the binary boundaries', () => {
    expect(formatKb(0)).toBe('0 KB');
    expect(formatKb(1023)).toBe('1023 KB');
    expect(formatKb(1024)).toBe('1.0 MB');
    expect(formatKb(1_048_575)).toBe('1024.0 MB');
    expect(formatKb(1_048_576)).toBe('1.0 GB');
    expect(formatKb(3_670_016)).toBe('3.5 GB');
  });

  it('rounds a fractional KB reading rather than showing decimals', () => {
    expect(formatKb(12.4)).toBe('12 KB');
    expect(formatKb(12.6)).toBe('13 KB');
  });

  it('keeps one decimal above the KB step, unlike formatBytes', () => {
    // Pinned because this helper is slated to move into `lib/format.ts`: `formatBytes(2048 * 1024)`
    // renders "2 MB", and folding it in must not silently change what the Meraki card shows.
    expect(formatKb(2048)).toBe('2.0 MB');
  });
});
