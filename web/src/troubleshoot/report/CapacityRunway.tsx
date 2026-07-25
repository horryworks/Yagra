// SPDX-License-Identifier: AGPL-3.0-only
// Capacity runway sparkline — capacity's answer to the anomaly chart. Plots the projected trend from
// the resource's current utilization toward the 100% ceiling: the shaded "next 30 days" band, the
// dashed ceiling, the trend line, and a marker where it crosses 100%.
//
// A number pair ("0%, 0.6%/day") does not read as urgency; a line arriving at a wall does. Same
// approach as AnomalyChart (small static inline SVG, colours via CSS classes) — but the layout maths
// is split into a PURE builder so its edge cases (flat/negative slope, already-full, horizon clamp)
// are unit-tested without a DOM, following the FlowSankey pattern.

const W = 300;
const H = 60;
const PADX = 2;
const PADT = 6;
const PADB = 6;

/** Longest runway we bother drawing — the backend already discards anything past a year. */
export const MAX_HORIZON_DAYS = 365;
/** The near-term band operators actually plan against. */
export const NEAR_TERM_DAYS = 30;

export interface RunwayDetail {
  /** Current utilization percent. */
  current: number;
  /** Least-squares growth in percentage points per day. */
  slope_per_day: number;
  /** Projected days until 100%. */
  tte_days: number;
}

export interface RunwayModel {
  /** Plot width/height in viewBox units. */
  w: number;
  h: number;
  /** The trend line as an SVG path. */
  line: string;
  /** y of the 100% ceiling. */
  ceilingY: number;
  /** Width of the leading "next 30 days" band (0 when the horizon is shorter). */
  nearW: number;
  /** Where the trend reaches 100%, or `null` when it never does inside the horizon. */
  cross: { x: number; y: number } | null;
  /** Days spanned by the x axis. */
  horizonDays: number;
}

/**
 * Build the runway geometry. Returns `null` when there is nothing meaningful to draw.
 *
 * Deliberate behaviours (pinned by tests):
 * - `slope <= 0` ⇒ a flat line and **no crossing** — never divide by a non-positive slope.
 * - `current >= 100` ⇒ already exhausted; the crossing sits at x = 0.
 * - the horizon is `tte * 1.2` so the crossing lands inside the plot with headroom, clamped to
 *   [`NEAR_TERM_DAYS`, `MAX_HORIZON_DAYS`] so a 3-day runway is still readable and a 300-day one
 *   doesn't flatten to nothing.
 */
export function buildRunway(d: RunwayDetail): RunwayModel | null {
  const { current, slope_per_day: slope } = d;
  if (!Number.isFinite(current) || !Number.isFinite(slope)) return null;

  const tte = Number.isFinite(d.tte_days) && d.tte_days > 0 ? d.tte_days : NEAR_TERM_DAYS;
  const horizonDays = Math.min(MAX_HORIZON_DAYS, Math.max(NEAR_TERM_DAYS, tte * 1.2));

  const plotW = W - PADX * 2;
  const plotH = H - PADT - PADB;
  // y is a fixed 0–100% scale: the ceiling means the same thing on every row, so rows are comparable.
  const yAt = (pct: number) => PADT + plotH - (Math.max(0, Math.min(100, pct)) / 100) * plotH;
  const xAt = (day: number) => PADX + (Math.max(0, Math.min(horizonDays, day)) / horizonDays) * plotW;

  const endPct = current + slope * horizonDays;
  const line = `M${xAt(0).toFixed(1)} ${yAt(current).toFixed(1)} L${xAt(horizonDays).toFixed(1)} ${yAt(endPct).toFixed(1)}`;

  let cross: { x: number; y: number } | null = null;
  if (current >= 100) {
    cross = { x: xAt(0), y: yAt(100) };
  } else if (slope > 0) {
    const days = (100 - current) / slope;
    if (days <= horizonDays) cross = { x: xAt(days), y: yAt(100) };
  }

  return {
    w: W,
    h: H,
    line,
    ceilingY: yAt(100),
    nearW: Math.max(0, xAt(Math.min(NEAR_TERM_DAYS, horizonDays)) - PADX),
    cross,
    horizonDays,
  };
}

export function CapacityRunway({
  detail,
  severity,
}: {
  detail: RunwayDetail;
  severity: 'crit' | 'warn' | 'info';
}) {
  const m = buildRunway(detail);
  if (!m) return null;
  return (
    <svg viewBox={`0 0 ${m.w} ${m.h}`} preserveAspectRatio="none" role="img" aria-hidden>
      {m.nearW > 0 && (
        <rect
          className="tsr-runway-near"
          x={PADX}
          y={PADT}
          width={m.nearW.toFixed(1)}
          height={H - PADT - PADB}
        />
      )}
      <line
        className="tsr-runway-ceiling"
        x1={PADX}
        y1={m.ceilingY.toFixed(1)}
        x2={W - PADX}
        y2={m.ceilingY.toFixed(1)}
      />
      <path className={`tsr-runway-line ${severity}`} d={m.line} />
      {m.cross && (
        <circle
          className={`tsr-runway-cross ${severity}`}
          cx={m.cross.x.toFixed(1)}
          cy={m.cross.y.toFixed(1)}
          r={3}
        />
      )}
    </svg>
  );
}
