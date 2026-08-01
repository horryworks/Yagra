// SPDX-License-Identifier: AGPL-3.0-only
// Chart-series arithmetic. Both functions decide what an operator *sees* on a graph, and both
// failure modes are silent: a gap drawn as zero looks like an outage, and an unmatched pair
// interpolated across polls looks like a real percentage.

import { describe, expect, it } from 'vitest';
import { alignTo, pctSeries } from './seriesMath';

describe('alignTo', () => {
  it('fills a missing reading with null so the chart draws a gap, not a cliff to zero', () => {
    expect(alignTo([1, 2, 3], [{ t: 1, v: 10 }, { t: 3, v: 30 }])).toEqual([10, null, 30]);
  });

  it('keeps a genuine zero distinct from a missing point', () => {
    // 0% CPU is a real reading; it must not be indistinguishable from "no poll".
    expect(alignTo([1, 2], [{ t: 1, v: 0 }])).toEqual([0, null]);
  });

  it('ignores readings outside the base axis', () => {
    expect(alignTo([2], [{ t: 1, v: 10 }, { t: 2, v: 20 }, { t: 3, v: 30 }])).toEqual([20]);
  });

  it('returns an all-null row for a series with no readings at all', () => {
    expect(alignTo([1, 2], [])).toEqual([null, null]);
  });
});

describe('pctSeries', () => {
  it('pairs used and size on shared timestamps only', () => {
    const out = pctSeries(
      [{ t: 1, v: 25 }, { t: 2, v: 50 }, { t: 3, v: 75 }],
      [{ t: 1, v: 100 }, { t: 3, v: 150 }],
    );
    // t=2 has no size reading, so it is dropped rather than paired with t=1's or t=3's.
    expect(out.timestamps).toEqual([1, 3]);
    expect(out.values).toEqual([25, 50]);
  });

  it('drops a non-positive size instead of emitting Infinity or NaN', () => {
    // A zero-size filesystem is a bogus reading; dividing by it produces a value uPlot still draws.
    const out = pctSeries([{ t: 1, v: 5 }, { t: 2, v: 5 }], [{ t: 1, v: 0 }, { t: 2, v: -1 }]);
    expect(out).toEqual({ timestamps: [], values: [] });
  });

  it('reports a full disk as 100 and an empty one as 0', () => {
    expect(pctSeries([{ t: 1, v: 100 }], [{ t: 1, v: 100 }]).values).toEqual([100]);
    expect(pctSeries([{ t: 1, v: 0 }], [{ t: 1, v: 100 }]).values).toEqual([0]);
  });

  it('is empty when either side has no readings', () => {
    expect(pctSeries([], [{ t: 1, v: 100 }])).toEqual({ timestamps: [], values: [] });
    expect(pctSeries([{ t: 1, v: 1 }], [])).toEqual({ timestamps: [], values: [] });
  });
});
