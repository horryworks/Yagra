// SPDX-License-Identifier: AGPL-3.0-only
// The chart series palette, kept apart from `MetricChart.tsx` so that file exports only a component
// (react-refresh's rule: a `.tsx` that also exports values cannot be hot-reloaded in place).

/** Default series palette (In / Out / aux …), indexed by series position, as theme tokens. In a DOM
 *  or SVG (via inline `style`/CSS) `var(--series-N)` resolves directly; passed to MetricChart as a
 *  series `color` it's resolved against computed style at build (canvas can't read CSS vars). One
 *  source of truth so legend swatches mirror the chart instead of re-hardcoding a hex. */
export const PALETTE = [
  'var(--series-1)',
  'var(--series-2)',
  'var(--series-3)',
  'var(--series-4)',
  'var(--series-5)',
  'var(--series-6)',
];
/** Canonical In / Out series colors (used by both the chart strokes and the legend swatches). */
export const SERIES_IN = PALETTE[0];
export const SERIES_OUT = PALETTE[1];
