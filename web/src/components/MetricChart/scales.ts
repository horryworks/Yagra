// SPDX-License-Identifier: AGPL-3.0-only
import uPlot from 'uplot';

/** The axis windows a chart is pinned to *right now* — each absent when that axis is free to
 *  auto-fit its data. Read on every scale pass, so a window that changes, or is dropped, lands on
 *  the next data update without rebuilding the plot. */
export interface ChartPins {
  xRange?: [number, number];
  yRange?: [number, number];
}

/** Build the uPlot `scales` option from a getter for the caller's *current* axis pins.
 *
 *  A fixed `xRange` (`[from, to]` unix seconds) pins the time axis to the requested window, so a
 *  chart whose data doesn't fill that window renders the full span — the missing portion stays
 *  visible as empty axis — instead of auto-fitting to the data extent. This is what makes "a
 *  newly-added node only has 3 days of history" visually distinct from "the 7d selector is broken":
 *  the axis shows 7 days either way, with the line simply starting partway in. `yRange` pins the
 *  value axis the same way (e.g. 0–100% gauges, so the baseline is 0 rather than the data's min).
 *
 *  🚨 **Both are functions, never the `[min, max]` array uPlot also accepts.** An array does not
 *  mean "the range now", it means "the range forever": uPlot wraps it in a constant function
 *  (`fnOrSelf`), sets `auto = false`, and — for the x scale alone — re-runs that function on every
 *  scale pass, overwriting whatever `setScale('x', …)` asked for. Since `MetricChart` rebuilds its
 *  instance only on a structural change, the window in force when a chart first mounted survived
 *  every range switch and every poll tick, and a relative window never advanced with the clock.
 *  That is ADR-117, and it is why the range selector looked broken on Overview while Interfaces —
 *  which happens to blank its data on a range change, remounting the chart — looked fine.
 *  `scales.test.ts` keeps the trap itself as a negative control.
 *
 *  With an axis unpinned, each fallback reproduces uPlot's own default exactly — `snapNumX` for the
 *  time axis, `snapNumY` (`rangeNum` at uPlot's `rangePad`) for the value axis — so an unpinned
 *  chart lands where it would with no `scales` option at all. That equivalence is what lets a pin be
 *  *released* (the `auto` half of the Interfaces throughput chart's Bandwidth ⇄ Auto toggle), and
 *  the parity test that asserts it is the only thing guarding the `0.1` copied out of uPlot, which
 *  does not export `rangePad`.
 *
 *  ⚠️ This module imports uPlot as a **value** (for `rangeNum`), so importing it loads uPlot — which
 *  touches `matchMedia` at module scope. A test that reaches this file needs a DOM. */
export function buildChartScales(pins: () => ChartPins): NonNullable<uPlot.Options['scales']> {
  // uPlot's `.d.ts` types the data bounds as `number`, but they *are* null on an empty chart — its
  // own defaults test for exactly that. Widening them here is safe: parameters are contravariant,
  // so this is still assignable to `uPlot.Scale.Range`.
  const x = (_u: uPlot, min: number | null, max: number | null): uPlot.Range.MinMax =>
    pins().xRange ?? (min == null || max == null ? [null, null] : [min, max]);
  const y = (_u: uPlot, min: number | null, max: number | null): uPlot.Range.MinMax =>
    pins().yRange ??
    (min == null || max == null ? [null, null] : uPlot.rangeNum(min, max, 0.1, true));
  return { x: { time: true, range: x }, y: { range: y } };
}
