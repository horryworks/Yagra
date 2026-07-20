// SPDX-License-Identifier: AGPL-3.0-only
import type uPlot from 'uplot';

/** Build the uPlot `scales` option from optional fixed axis ranges.
 *
 *  A fixed `xRange` (`[from, to]` unix seconds) pins the time axis to the *requested* window,
 *  so a chart whose data doesn't fill that window renders the full span — the missing portion
 *  stays visible as empty axis — instead of auto-fitting to the data extent. This is what makes
 *  "a newly-added node only has 3 days of history" visually distinct from "the 7d selector is
 *  broken": the axis shows 7 days either way, with the line simply starting partway in.
 *
 *  `yRange` pins the value axis (e.g. 0–100% gauges so the baseline is 0, not the data's min).
 *  Returns `undefined` when neither is set, so uPlot auto-fits both axes (the default). */
export function buildChartScales(
  xRange?: [number, number],
  yRange?: [number, number],
): uPlot.Options['scales'] {
  if (!xRange && !yRange) return undefined;
  const scales: NonNullable<uPlot.Options['scales']> = {};
  if (xRange) scales.x = { time: true, range: xRange };
  if (yRange) scales.y = { range: yRange };
  return scales;
}
