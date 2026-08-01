// SPDX-License-Identifier: AGPL-3.0-only
// Time-series alignment and percentage derivation, shared by the chart surfaces.
//
// Extracted from the `.tsx` files that used to hold private copies (SystemHealthPage's host graphs
// and the node Overview's memory gauge), because this is exactly the arithmetic ui-conventions
// warns about: a chart must handle gaps and counter resets "without drawing misleading spikes",
// and a missing point silently becoming a zero is how that happens. Vitest never runs `.tsx`, so
// while it lived there it could not be tested.

/** One `(timestamp, value)` reading, matching the metric API's point shape. */
export interface TimedPoint {
  t: number;
  v: number;
}

/** Align one series onto a base timestamp axis.
 *
 *  A timestamp the series has no reading for becomes `null` — **not** `0`. uPlot renders a null as
 *  a gap, which is the truth ("we did not poll"); a zero would draw a cliff to the floor and back,
 *  which reads as an outage that never happened. */
export function alignTo(base: number[], points: TimedPoint[]): (number | null)[] {
  const byT = new Map(points.map((p) => [p.t, p.v]));
  return base.map((t) => byT.get(t) ?? null);
}

/** Derive a used-% series from two raw series, keeping only timestamps present in both.
 *
 *  A used reading with no matching size is dropped rather than paired with a neighbouring size:
 *  the two are separate polls, and interpolating across them would invent a percentage. A
 *  non-positive size is dropped too — dividing by it yields Infinity or NaN, which uPlot draws. */
export function pctSeries(
  used: TimedPoint[],
  size: TimedPoint[],
): { timestamps: number[]; values: number[] } {
  const sizeByT = new Map(size.map((p) => [p.t, p.v]));
  const timestamps: number[] = [];
  const values: number[] = [];
  for (const p of used) {
    const s = sizeByT.get(p.t);
    if (s == null || s <= 0) continue;
    timestamps.push(p.t);
    values.push((p.v / s) * 100);
  }
  return { timestamps, values };
}
