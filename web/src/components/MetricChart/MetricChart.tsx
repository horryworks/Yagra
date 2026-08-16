// SPDX-License-Identifier: AGPL-3.0-only
// Time-series chart pane backed by uPlot (fast at many series). Series colors come from the
// theme's `--series-*` tokens (the same categorical palette every other chart surface uses), so
// charts adapt to light/dark and never collide with the status channel. A uPlot canvas stroke
// can't read a CSS var directly, so MetricChart resolves the `var(--series-N)` tokens against the
// element's computed style at build time (like it already does for the axis/grid/reference colors).
// Supports a single series (`values`) or multiple (`series`), and resizes to its container width.

import { useEffect, useRef } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import { buildChartScales } from './scales';
import { idleLegendIdx } from './legend';
import './MetricChart.css';

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

/** Static fallbacks for the `--series-*` tokens, used only if computed style can't resolve them
 *  (e.g. the var is missing). Mirror tokens.css so a fallback still looks right. */
const SERIES_FALLBACK = ['#4c8dd6', '#9b7bd4', '#4caf9b', '#d68a4c', '#b05c8e', '#7c9b4c'];

/** Resolve a color that may be a `var(--token)` reference against `cs` (the element's computed
 *  style). Non-var strings pass through unchanged; an unresolvable var falls back. */
function resolveColor(
  raw: string | undefined,
  cs: CSSStyleDeclaration,
  fallback: string,
): string {
  if (!raw) return fallback;
  const m = raw.match(/^var\((--[\w-]+)\)$/);
  if (!m) return raw;
  return cs.getPropertyValue(m[1]).trim() || fallback;
}

// First-paint estimate of uPlot's title+legend chrome height (px) in `fill` mode, before the real
// elements exist to measure. Corrected to the exact measured value immediately after construction.
const FILL_CHROME_ESTIMATE = 28;
const MIN_PLOT_HEIGHT = 40;

export interface ChartSeries {
  label: string;
  /** Y values aligned to `timestamps`; `null` renders a gap. */
  values: (number | null)[];
  /** Stroke color; falls back to the palette by index. */
  color?: string;
}

interface Props {
  title: string;
  /** X axis: Unix seconds. */
  timestamps: number[];
  /** Single-series convenience. Ignored when `series` is given. */
  values?: number[];
  /** Multiple series (takes precedence over `values`). */
  series?: ChartSeries[];
  /** Chart height in px (default 220). Ignored when `fill` is set. */
  height?: number;
  /** Fill the container's height instead of using a fixed `height` — the chart tracks the pane's
   *  height (as well as its width) via the ResizeObserver. Used by dashboard widgets whose cell can
   *  be resized taller. The wrapper must have a definite height (a flex/grid child that stretches). */
  fill?: boolean;
  /** Optional Y-axis tick formatter (e.g. SI suffixes so big numbers don't clip). */
  yFormat?: (v: number) => string;
  /** Fixed Y-axis `[min, max]`; omit to auto-fit the data (uPlot default). Bounded gauges like
   *  CPU/Mem % pass `[0, 100]` so the baseline is 0, not the data's min. Pass a stable reference
   *  (e.g. a module-level constant) so the chart isn't rebuilt on every render. */
  yRange?: [number, number];
  /** Fixed X-axis `[from, to]` in unix seconds — pins the time window so a chart whose data
   *  doesn't fill the requested range renders the full window (the empty span stays visible)
   *  instead of auto-fitting to the data extent. Omit to auto-fit (uPlot default). Pass a stable
   *  reference (e.g. captured alongside the fetched series) so the chart isn't rebuilt on every
   *  render. See `buildChartScales`. */
  xRange?: [number, number];
  /** Formatter for the cursor-legend value (the "Value" readout on hover). Use it to show a unit
   *  the compact axis omits (e.g. ms / bps). Falls back to `yFormat` when not given. */
  legendFormat?: (v: number) => string;
  /** Optional horizontal reference line (e.g. an interface's configured bandwidth) drawn in the
   *  `--status-critical` colour. When the value sits above the visible Y range it is pinned to the
   *  top edge and labelled, so it's always visible and slides into the plot as data nears it. */
  referenceLine?: { value: number; label?: string };
  /** Share the cursor with every other chart given the same key: hovering one moves the crosshair
   *  and the live legend of all of them, so charts stacked over the same time window can be read at
   *  a single instant instead of one at a time. Only the cursor position is shared — series
   *  toggling and focus stay per chart, because synced charts plot different series and matching
   *  them by position would toggle an unrelated line. Omit for an independent chart. */
  syncKey?: string;
}

/** Pin the live legend to the most recent sample whenever the cursor is away, so an unhovered
 *  chart reports its current values instead of a column of `--`. Runs after uPlot has set the
 *  legend itself (the `setCursor` hook fires at the end of `updateCursor`), and only when uPlot has
 *  no index of its own — a real hover, including one arriving over `syncKey`, always wins. */
function applyIdleLegend(u: uPlot) {
  if (u.cursor.idx != null) return;
  const idx = idleLegendIdx(u.data.slice(1) as (number | null)[][]);
  if (idx != null) u.setLegend({ idx }, false);
}

export function MetricChart({
  title,
  timestamps,
  values,
  series,
  height = 220,
  fill = false,
  yFormat,
  yRange,
  xRange,
  legendFormat,
  referenceLine,
  syncKey,
}: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const plotRef = useRef<uPlot | null>(null);
  // Latest render-varying props, read by the uPlot option closures at draw time. This is what
  // lets a poll tick refresh data WITHOUT rebuilding the chart: fresh inline formatters / a fresh
  // `referenceLine` object each render don't change the instance, only what its closures read.
  const live = useRef({ yFormat, legendFormat, referenceLine });
  live.current = { yFormat, legendFormat, referenceLine };

  const resolved: ChartSeries[] =
    series ?? (values ? [{ label: title, values, color: PALETTE[0] }] : []);
  // A full rebuild is needed only when the chart *shape* changes (title, height, series
  // count/labels/colors) — not when the data values, axis ranges, or formatters change. Those
  // update the existing instance in place (see the data effect below).
  const structKey =
    `${title}|${height}|${syncKey ?? ''}|` +
    resolved.map((s) => `${s.label}:${s.color ?? ''}`).join('|');
  // Content signature of the optional reference line, so a value/label change redraws in place.
  const refKey = referenceLine ? `${referenceLine.value}:${referenceLine.label ?? ''}` : '';

  // ── Create (or structurally rebuild) the uPlot instance ──────────────────────────────────
  useEffect(() => {
    const el = ref.current;
    if (!el) return;

    // Canvas can't read CSS variables, so resolve the theme's axis/grid colors here (adapts
    // to light/dark). The instance is rebuilt on a structural/theme-affecting change, so a theme
    // switch is picked up then (and on the next data tick's redraw via the live refs).
    const cs = getComputedStyle(el);
    const axisColor = cs.getPropertyValue('--text-tertiary').trim() || '#8a8f98';
    const gridColor = cs.getPropertyValue('--border-color').trim() || 'rgba(255,255,255,0.1)';
    const refColor = cs.getPropertyValue('--status-critical').trim() || '#ef5350';
    const uiFont = cs.getPropertyValue('--ui-font-family').trim() || 'sans-serif';
    // Resolve the theme's categorical series palette once for this build (canvas needs concrete
    // colors). A series' own `color` (which may itself be a `var(--series-N)`) wins over the palette.
    const pal = PALETTE.map((v, i) => resolveColor(v, cs, SERIES_FALLBACK[i % SERIES_FALLBACK.length]));
    const axis = {
      stroke: axisColor,
      grid: { stroke: gridColor, width: 1 },
      ticks: { stroke: gridColor, width: 1 },
    };
    const yAxis = {
      ...axis,
      // Compact Y tick labels (e.g. SI suffixes) so wide numbers aren't clipped. Reads the live
      // formatter so a formatter swap is picked up on the next redraw without a rebuild.
      values: (_u: uPlot, splits: number[]) =>
        splits.map((s) => {
          const f = live.current.yFormat;
          return s == null ? '' : f ? f(s) : String(s);
        }),
    };

    const opts: uPlot.Options = {
      title,
      width: el.clientWidth || 460,
      height: fill
        ? Math.max(MIN_PLOT_HEIGHT, (el.clientHeight || height) - FILL_CHROME_ESTIMATE)
        : height,
      axes: [axis, yAxis],
      // Force a fixed Y range (e.g. 0–100% gauges) and/or X window (pin to the requested time
      // range) when asked; otherwise uPlot auto-fits the respective axis to the data.
      scales: buildChartScales(xRange, yRange),
      ...(syncKey ? { cursor: { sync: { key: syncKey, setSeries: false } } } : {}),
      series: [
        {},
        ...resolved.map((s, i) => ({
          label: s.label,
          stroke: s.color ? resolveColor(s.color, cs, pal[i % pal.length]) : pal[i % pal.length],
          width: 2,
          // Cursor-legend "Value" readout — format with units so a hover reads "75%" / "12 ms",
          // not a bare number. Reads the live formatters (explicit legend formatter, else the axis
          // one) so a formatter swap is picked up on redraw without rebuilding the chart.
          value: (_u: uPlot, v: number | null) => {
            const f = live.current.legendFormat ?? live.current.yFormat;
            return v == null ? '--' : f ? f(v) : `${v}`;
          },
        })),
      ],
      // Horizontal reference line (e.g. configured bandwidth), drawn over the series. Always
      // installed and reads the *live* reference line, so it can appear/disappear/change between
      // data ticks without rebuilding the chart. Canvas works in device pixels, so scale
      // stroke/text by the pixel ratio. When the value sits inside the visible range it's drawn at
      // its true Y; when it's off-scale the line is pinned to the nearest edge and its label gets a
      // ↑/↓ marker — so the operator can still see the threshold and that the real value is beyond
      // the edge, rather than the line silently vanishing.
      hooks: {
        draw: [
          (u: uPlot) => {
            const referenceLine = live.current.referenceLine;
            if (!referenceLine) return;
            const refv = referenceLine.value;
            if (!Number.isFinite(refv)) return;
            const { left, top, width, height } = u.bbox;
                const trueY = u.valToPos(refv, 'y', true);
                const aboveRange = trueY < top; // value greater than the visible max
                const belowRange = trueY > top + height; // value less than the visible min
                const y = Math.min(top + height, Math.max(top, trueY)); // pin to the edge
                const ctx = u.ctx;
                const dpr = Math.max(1, Math.round(window.devicePixelRatio || 1));
                ctx.save();
                ctx.strokeStyle = refColor;
                ctx.lineWidth = 1.5 * dpr;
                ctx.setLineDash([5 * dpr, 4 * dpr]);
                ctx.beginPath();
                ctx.moveTo(left, y);
                ctx.lineTo(left + width, y);
                ctx.stroke();
                if (referenceLine.label) {
                  ctx.setLineDash([]);
                  ctx.fillStyle = refColor;
                  ctx.font = `${11 * dpr}px ${uiFont}`;
                  ctx.textAlign = 'left';
                  // Mark an off-scale threshold so a pinned line is never read as the true value.
                  const label = aboveRange
                    ? `↑ ${referenceLine.label}`
                    : belowRange
                      ? `↓ ${referenceLine.label}`
                      : referenceLine.label;
                  // Keep the label inside the plot: below the line when it's near the top edge,
                  // above it otherwise.
                  const nearTop = y - top < 14 * dpr;
                  ctx.textBaseline = nearTop ? 'top' : 'bottom';
                  ctx.fillText(label, left + 4 * dpr, y + (nearTop ? 2 * dpr : -2 * dpr));
                }
                ctx.restore();
              },
            ],
        // Both hooks exist to put the latest values back after uPlot has blanked the legend:
        // `setCursor` covers the cursor leaving (and a synced neighbour's leaving), `setData` the
        // refresh tick. Neither can loop — `setLegend(_, false)` fires no hook.
        setCursor: [applyIdleLegend],
        setData: [applyIdleLegend],
      },
    };
    const data = [timestamps, ...resolved.map((s) => s.values)] as uPlot.AlignedData;
    const plot = new uPlot(opts, data, el);
    plotRef.current = plot;
    applyIdleLegend(plot);

    // Resize the plot to its pane. Width always tracks the container; height tracks too in `fill`
    // mode (a resizable dashboard cell). CRITICAL: in fill mode the plot height must be the pane
    // height MINUS uPlot's own title+legend chrome — those render as extra DOM below the plot, so
    // sizing the plot to the full pane makes `.uplot` taller than the pane, card-body shows a
    // scrollbar, the scrollbar shrinks the pane, and the ResizeObserver oscillates (flickering
    // scrollbar + doubled baseline). Deferring to rAF also avoids the synchronous RO loop.
    let raf = 0;
    const chromeHeight = () => {
      const title = el.querySelector<HTMLElement>('.u-title');
      const legend = el.querySelector<HTMLElement>('.u-legend');
      return (title?.offsetHeight ?? 0) + (legend?.offsetHeight ?? 0);
    };
    const applySize = () => {
      const w = el.clientWidth;
      if (w <= 0) return;
      const h = fill ? Math.max(MIN_PLOT_HEIGHT, el.clientHeight - chromeHeight()) : height;
      plot.setSize({ width: w, height: h });
    };
    // Correct the first-paint estimate now that the real chrome elements are in the DOM.
    if (fill) applySize();

    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(applySize);
    });
    ro.observe(el);

    return () => {
      cancelAnimationFrame(raf);
      ro.disconnect();
      plot.destroy();
      plotRef.current = null;
    };
    // Rebuild only on a structural/theme-affecting change; data & ranges update in place below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [structKey, height]);

  // ── Update data, axis ranges, and the reference line in place (no rebuild per poll tick) ──
  useEffect(() => {
    const plot = plotRef.current;
    if (!plot) return;
    const data = [timestamps, ...resolved.map((s) => s.values)] as uPlot.AlignedData;
    // setData (resetScales=true) auto-fits both axes; then re-pin any axis the caller fixed.
    plot.setData(data);
    if (xRange) plot.setScale('x', { min: xRange[0], max: xRange[1] });
    if (yRange) plot.setScale('y', { min: yRange[0], max: yRange[1] });
    // `live` already holds the current formatters / reference line; setData triggered the redraw.
    // Keyed on data + range/refline *content* so a parent re-render with unchanged data is a no-op.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [timestamps, values, series, xRange, yRange, refKey, structKey]);

  return <div ref={ref} className={fill ? 'metricchart-fill' : undefined} />;
}
