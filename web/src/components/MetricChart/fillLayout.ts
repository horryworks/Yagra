// SPDX-License-Identifier: AGPL-3.0-only
// How a `fill` chart splits its pane between the plot and uPlot's legend.
//
// The plot used to get whatever the legend left. A chart with many series has a legend that wraps
// to many lines, so an Interface traffic card with six links (twelve legend entries) gave the plot
// its 40px floor — the axes alone — and the time ticks were drawn over the lines (found on a lab
// deployment, 2026-10-08). The legend is now capped at a share of the pane and scrolls inside that
// cap, so the plot keeps the larger part whatever the series count.

/** The smallest plot (axes included) the chart will draw, in px. */
export const MIN_PLOT_HEIGHT = 40;

/** The largest share of the pane (after the title) the legend may take before it scrolls. */
export const LEGEND_MAX_SHARE = 0.35;

/** The legend is never capped below one line of entries, or a two-series chart would scroll. */
export const LEGEND_MIN_CAP = 24;

export interface FillSplit {
  /** The `max-height` to give the legend, in px. */
  legendCap: number;
  /** The height to hand uPlot for the plot, in px. */
  plotHeight: number;
}

/**
 * Split `paneHeight` between the plot and a legend whose unconstrained height is `legendNatural`.
 *
 * The legend takes what it needs up to its cap; the plot takes the rest, never below
 * [`MIN_PLOT_HEIGHT`] — the floor is the outer bound, so a pane too small for both still draws axes
 * (the same rule the resize handles follow).
 */
export function fillSplit(paneHeight: number, titleHeight: number, legendNatural: number): FillSplit {
  const body = Math.max(0, paneHeight - titleHeight);
  const legendCap = Math.max(LEGEND_MIN_CAP, Math.floor(body * LEGEND_MAX_SHARE));
  const legend = Math.min(Math.max(0, legendNatural), legendCap);
  return { legendCap, plotHeight: Math.max(MIN_PLOT_HEIGHT, body - legend) };
}
