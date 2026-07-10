// Time-series chart pane backed by uPlot (fast at many series). Canvas strokes can't read
// CSS vars, so chart colors come from a small palette constant — exempt from the theme-var
// rule per CLAUDE.md (chart-library color props are data-driven). Supports a single series
// (`values`) or multiple (`series`), and resizes to its container width.

import { useEffect, useRef } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import { buildChartScales } from './scales';
import './MetricChart.css';

/** Default series palette (In / Out / aux …), indexed by series position. Canvas strokes can't
 *  read CSS vars, so this data-driven palette is the single source of truth for series colors —
 *  exported so DOM legend swatches mirror the chart instead of re-hardcoding the same hex. */
export const PALETTE = ['#4f8cff', '#34d399', '#f59e0b', '#ef4444'];
/** Canonical In / Out series colors (used by both the chart strokes and the legend swatches). */
export const SERIES_IN = PALETTE[0];
export const SERIES_OUT = PALETTE[1];

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
    `${title}|${height}|` + resolved.map((s) => `${s.label}:${s.color ?? ''}`).join('|');
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
      series: [
        {},
        ...resolved.map((s, i) => ({
          label: s.label,
          stroke: s.color ?? PALETTE[i % PALETTE.length],
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
          },
    };
    const data = [timestamps, ...resolved.map((s) => s.values)] as uPlot.AlignedData;
    const plot = new uPlot(opts, data, el);
    plotRef.current = plot;

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
