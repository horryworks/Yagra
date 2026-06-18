// Time-series chart pane backed by uPlot (fast at many series). Canvas strokes can't read
// CSS vars, so chart colors come from a small palette constant — exempt from the theme-var
// rule per CLAUDE.md (chart-library color props are data-driven). Supports a single series
// (`values`) or multiple (`series`), and resizes to its container width.

import { useEffect, useRef } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';
import './MetricChart.css';

/** Default series palette (In / Out / aux …), indexed by series position. */
const PALETTE = ['#4f8cff', '#34d399', '#f59e0b', '#ef4444'];

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
  /** Chart height in px (default 220). */
  height?: number;
  /** Optional Y-axis tick formatter (e.g. SI suffixes so big numbers don't clip). */
  yFormat?: (v: number) => string;
  /** Fixed Y-axis `[min, max]`; omit to auto-fit the data (uPlot default). Bounded gauges like
   *  CPU/Mem % pass `[0, 100]` so the baseline is 0, not the data's min. Pass a stable reference
   *  (e.g. a module-level constant) so the chart isn't rebuilt on every render. */
  yRange?: [number, number];
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
  yFormat,
  yRange,
  legendFormat,
  referenceLine,
}: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const resolved: ChartSeries[] =
      series ?? (values ? [{ label: title, values, color: PALETTE[0] }] : []);

    // Canvas can't read CSS variables, so resolve the theme's axis/grid colors here (adapts
    // to light/dark). Charts re-create on data refresh, so a theme switch is picked up then.
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
    const yAxis = yFormat
      ? {
          ...axis,
          // Compact Y tick labels (e.g. SI suffixes) so wide numbers aren't clipped.
          values: (_u: uPlot, splits: number[]) =>
            splits.map((s) => (s == null ? '' : yFormat(s))),
        }
      : axis;
    // Cursor-legend value formatter: an explicit one (units the axis omits) else the axis one.
    const legendFmt = legendFormat ?? yFormat;

    const opts: uPlot.Options = {
      title,
      width: el.clientWidth || 460,
      height,
      axes: [axis, yAxis],
      // Force a fixed Y range when asked (e.g. 0–100% gauges); otherwise uPlot auto-fits.
      scales: yRange ? { y: { range: yRange } } : undefined,
      series: [
        {},
        ...resolved.map((s, i) => ({
          label: s.label,
          stroke: s.color ?? PALETTE[i % PALETTE.length],
          width: 2,
          // Cursor-legend "Value" readout — format with units so a hover reads "75%" / "12 ms",
          // not a bare number. Prefer an explicit legend formatter, else the axis formatter.
          value: (_u: uPlot, v: number | null) =>
            v == null ? '--' : legendFmt ? legendFmt(v) : `${v}`,
        })),
      ],
      // Horizontal reference line (e.g. configured bandwidth), drawn over the series. Canvas works
      // in device pixels, so scale stroke/text by the pixel ratio. When the value sits inside the
      // visible range it's drawn at its true Y; when it's off-scale (auto-fit mode with traffic
      // well below the bandwidth) the line is pinned to the nearest edge and its label gets a ↑/↓
      // marker — so the operator can still see the threshold and that the real value is beyond the
      // edge, rather than the line silently vanishing.
      hooks: referenceLine
        ? {
            draw: [
              (u: uPlot) => {
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
          }
        : undefined,
    };
    const data = [timestamps, ...resolved.map((s) => s.values)] as uPlot.AlignedData;
    const plot = new uPlot(opts, data, el);

    // Track the container width so the chart fills the pane (and reflows on layout change).
    const ro = new ResizeObserver(() => {
      const w = el.clientWidth;
      if (w > 0) plot.setSize({ width: w, height });
    });
    ro.observe(el);

    return () => {
      ro.disconnect();
      plot.destroy();
    };
  }, [title, timestamps, values, series, height, yFormat, yRange, legendFormat, referenceLine]);

  return <div ref={ref} />;
}
