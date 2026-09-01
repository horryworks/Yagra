// SPDX-License-Identifier: AGPL-3.0-only
// Scan-shape scatter: distinct destinations (x) against distinct destination ports (y), both on a log
// scale, with the two scan regions shaded and the dst = ports diagonal drawn.
//
// Why a scatter and not a bar: "4496 destinations × 20 ports" (a horizontal sweep looking for one
// open service) and "20 destinations × 85 ports" (a vertical probe mapping a few hosts) are different
// attacks, and a ranked bar of either axis alone cannot separate them. Position on this plot IS the
// classification the backend computes.
//
// Pure `buildScanPlot` + thin renderer (the FlowSankey pattern), so the log/bounds edge cases are
// unit-testable without a DOM.

const W = 320;
const H = 200;
const PAD_L = 34;
const PAD_B = 24;
const PAD_T = 8;
const PAD_R = 8;

export interface ScanPoint {
  /** Source address — the plotted entity. */
  src: string;
  distinctDst: number;
  distinctPorts: number;
  severity: 'crit' | 'warn' | 'info';
}

export interface PlottedPoint extends ScanPoint {
  x: number;
  y: number;
}

export interface ScanPlotModel {
  w: number;
  h: number;
  points: PlottedPoint[];
  /** The dst = ports line, as an SVG path. Above it = vertical scan, below = horizontal. */
  diagonal: string;
  /** Axis tick labels with their positions. */
  xTicks: { v: number; x: number }[];
  yTicks: { v: number; y: number }[];
  /** Plot rect, for the region shading. */
  plot: { x: number; y: number; w: number; h: number };
}

/** log10 with a floor, so a zero/one count maps to the axis origin instead of -Infinity. */
/** Log10 with a floor at 1, so a source that touched a single host plots at the origin instead
 *  of at −∞. Exported for its test. */
export function lg(v: number): number {
  return Math.log10(Math.max(1, v));
}

/**
 * Lay out the scatter. Returns `null` when there is nothing to plot.
 *
 * The axes share one upper bound (the largest count on either axis, rounded up to the next power of
 * ten) so the dst = ports diagonal is a true 45° line and "which side am I on" reads correctly.
 */
export function buildScanPlot(points: ScanPoint[]): ScanPlotModel | null {
  if (!points.length) return null;

  const maxCount = Math.max(...points.flatMap((p) => [p.distinctDst, p.distinctPorts]), 10);
  // Round the log bound up to a whole decade so ticks are 1/10/100/… and the diagonal is symmetric.
  const decades = Math.max(1, Math.ceil(lg(maxCount)));

  const plotW = W - PAD_L - PAD_R;
  const plotH = H - PAD_T - PAD_B;
  const xAt = (v: number) => PAD_L + (lg(v) / decades) * plotW;
  const yAt = (v: number) => PAD_T + plotH - (lg(v) / decades) * plotH;

  const ticks = Array.from({ length: decades + 1 }, (_, i) => 10 ** i);

  return {
    w: W,
    h: H,
    points: points.map((p) => ({
      ...p,
      x: xAt(p.distinctDst),
      y: yAt(p.distinctPorts),
    })),
    // From the origin to the top-right corner: equal counts on both axes.
    diagonal: `M${PAD_L} ${PAD_T + plotH} L${PAD_L + plotW} ${PAD_T}`,
    xTicks: ticks.map((v) => ({ v, x: xAt(v) })),
    yTicks: ticks.map((v) => ({ v, y: yAt(v) })),
    plot: { x: PAD_L, y: PAD_T, w: plotW, h: plotH },
  };
}

export function ScanPlot({
  points,
  labels,
}: {
  points: ScanPoint[];
  /** Localized axis/region captions (the component takes no i18n dependency of its own). */
  labels: { x: string; y: string; horizontal: string; vertical: string };
}) {
  const m = buildScanPlot(points);
  if (!m) return null;
  return (
    <svg
      className="tsr-scanplot"
      viewBox={`0 0 ${m.w} ${m.h}`}
      role="img"
      aria-label={`${labels.x} / ${labels.y}`}
    >
      {/* Region shading: below the diagonal = many hosts (horizontal), above = many ports. */}
      <polygon
        className="tsr-scan-region horizontal"
        points={`${m.plot.x},${m.plot.y + m.plot.h} ${m.plot.x + m.plot.w},${m.plot.y + m.plot.h} ${m.plot.x + m.plot.w},${m.plot.y}`}
      />
      <polygon
        className="tsr-scan-region vertical"
        points={`${m.plot.x},${m.plot.y + m.plot.h} ${m.plot.x},${m.plot.y} ${m.plot.x + m.plot.w},${m.plot.y}`}
      />
      {m.xTicks.map((tk) => (
        <text key={`x${tk.v}`} className="tsr-scan-tick" x={tk.x} y={m.h - 8} textAnchor="middle">
          {tk.v}
        </text>
      ))}
      {m.yTicks.map((tk) => (
        <text
          key={`y${tk.v}`}
          className="tsr-scan-tick"
          x={PAD_L - 6}
          y={tk.y + 3}
          textAnchor="end"
        >
          {tk.v}
        </text>
      ))}
      <path className="tsr-scan-diagonal" d={m.diagonal} />
      <rect
        className="tsr-scan-frame"
        x={m.plot.x}
        y={m.plot.y}
        width={m.plot.w}
        height={m.plot.h}
      />
      {/* Region captions, so the shading is never the only cue (colour-alone is not allowed). */}
      <text className="tsr-scan-region-label" x={m.plot.x + m.plot.w - 4} y={m.plot.y + m.plot.h - 6} textAnchor="end">
        {labels.horizontal}
      </text>
      <text className="tsr-scan-region-label" x={m.plot.x + 4} y={m.plot.y + 10}>
        {labels.vertical}
      </text>
      {m.points.map((p) => (
        <circle
          key={p.src}
          className={`tsr-scan-dot ${p.severity}`}
          cx={p.x}
          cy={p.y}
          r={4}
        >
          <title>{`${p.src} — ${p.distinctDst} × ${p.distinctPorts}`}</title>
        </circle>
      ))}
    </svg>
  );
}
