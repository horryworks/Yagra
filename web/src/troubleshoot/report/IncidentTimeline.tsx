// SPDX-License-Identifier: AGPL-3.0-only
// Incident timeline — the cross-signal view behind `incident_correlate`.
//
// An incident finding is only emitted when ≥2 signals of ≥2 different kinds coincide on a node, so
// the payload is inherently a sequence, not a scalar: a metric anomaly, passive events, and the
// dominant flow, each with a timestamp. Three fixed lanes (metric / event / flow) laid on a shared
// time axis make the *ordering* legible — "the events started, then traffic shifted" is the whole
// point of running this analysis, and no bar or number conveys it.
//
// Pure `buildIncidentTimeline` + thin renderer (the FlowSankey pattern) so the awkward cases —
// a zero-length span, an unknown lane, unsorted input — are unit-tested without a DOM.

/** One signal as the backend stores it in `detail.timeline[]`. */
export interface TimelineSignal {
  /** Unix seconds. */
  at: number;
  /** `metric` | `event` | `flow`; anything else lands in the `other` lane rather than vanishing. */
  kind: string;
  /** Pre-composed technical label (metric name, trap/app name + message, or a flow summary). */
  label: string;
  /** 0–100 float — NOT a severity string. Mirrors the backend's `severity_for` thresholds. */
  severity: number;
  /**
   * The neighbour this signal came from, when the incident was corroborated across a topology
   * link (ADR-022 Increment 2). **Absent for the subject's own signals**, which is what keeps the
   * payload additive: a finding written before the expansion has no such key anywhere, and reads
   * exactly as it did.
   */
  node_id?: string;
  node_name?: string;
}

/**
 * How a signal reads in a list or a tooltip.
 *
 * A corroborating neighbour's signal is prefixed with its node, because an unattributed one is
 * actively misleading: the timeline would show more activity than the subject actually had, and an
 * operator would read another device's flow shift as this device's.
 */
export function signalLabel(s: Pick<TimelineSignal, 'label' | 'node_name'>): string {
  return s.node_name ? `${s.node_name}: ${s.label}` : s.label;
}

export type Lane = 'metric' | 'event' | 'flow' | 'other';

/** Lane order is fixed (cause → effect reading order), independent of what a given incident has. */
export const LANES: Lane[] = ['metric', 'event', 'flow', 'other'];

const W = 640;
const LANE_H = 28;
const PAD_L = 74;
const PAD_R = 12;
const PAD_T = 10;
const AXIS_H = 18;
/** Minimum x separation before two markers are nudged apart. */
const MIN_GAP = 9;

export interface PlottedSignal extends TimelineSignal {
  lane: Lane;
  x: number;
  y: number;
  /** Marker radius — bigger for a more severe signal. */
  r: number;
  tone: 'crit' | 'warn' | 'info';
}

export interface IncidentTimelineModel {
  w: number;
  h: number;
  /** Only the lanes this incident actually has, in canonical order. */
  lanes: { lane: Lane; y: number }[];
  signals: PlottedSignal[];
  plot: { x: number; y: number; w: number; h: number };
  /** First/last signal times (unix seconds) for the axis end labels. */
  from: number;
  to: number;
}

/** Mirrors the backend's `severity_for`: ≥90 crit, ≥75 warn, else info. */
export function signalTone(severity: number): 'crit' | 'warn' | 'info' {
  if (severity >= 90) return 'crit';
  if (severity >= 75) return 'warn';
  return 'info';
}

/** Map a signal's `kind` to its lane. An unrecognised kind gets the `other` lane, never dropped. */
export function laneOf(kind: string): Lane {
  return kind === 'metric' || kind === 'event' || kind === 'flow' ? kind : 'other';
}

/**
 * Lay out an incident's signals. Returns `null` when there is nothing to draw.
 *
 * Pinned behaviours: an all-same-timestamp incident has a **zero span** and must not divide by it
 * (signals spread evenly instead); an unrecognised `kind` gets its own lane rather than being
 * silently dropped; input order doesn't matter (the model sorts); markers closer than `MIN_GAP` are
 * nudged apart so a burst stays countable.
 */
export function buildIncidentTimeline(
  timeline: TimelineSignal[] | undefined,
): IncidentTimelineModel | null {
  const valid = (timeline ?? []).filter((s) => Number.isFinite(s.at));
  if (!valid.length) return null;

  const sorted = valid.slice().sort((a, b) => a.at - b.at);
  const from = sorted[0].at;
  const to = sorted[sorted.length - 1].at;
  const span = to - from;

  // Only the lanes present, in canonical order — an incident with no flow signal shows no flow lane.
  const present = LANES.filter((l) => sorted.some((s) => laneOf(s.kind) === l));
  const laneY = new Map<Lane, number>();
  present.forEach((l, i) => laneY.set(l, PAD_T + i * LANE_H + LANE_H / 2));

  const plotW = W - PAD_L - PAD_R;
  const plotH = present.length * LANE_H;
  const h = PAD_T + plotH + AXIS_H;

  const xAt = (at: number, idx: number) =>
    span > 0
      ? PAD_L + ((at - from) / span) * plotW
      : // Zero span: spread evenly rather than stacking every marker on one pixel.
        PAD_L + (sorted.length > 1 ? (idx / (sorted.length - 1)) * plotW : plotW / 2);

  // Nudge collisions apart per lane so a burst of events remains countable.
  const lastX = new Map<Lane, number>();
  const signals: PlottedSignal[] = sorted.map((s, i) => {
    const lane = laneOf(s.kind);
    let x = xAt(s.at, i);
    const prev = lastX.get(lane);
    if (prev !== undefined && x - prev < MIN_GAP) x = prev + MIN_GAP;
    x = Math.min(x, PAD_L + plotW);
    lastX.set(lane, x);
    const tone = signalTone(s.severity);
    return {
      ...s,
      lane,
      x,
      y: laneY.get(lane) ?? PAD_T,
      r: tone === 'crit' ? 5.5 : tone === 'warn' ? 4.5 : 3.5,
      tone,
    };
  });

  return {
    w: W,
    h,
    lanes: present.map((lane) => ({ lane, y: laneY.get(lane) ?? PAD_T })),
    signals,
    plot: { x: PAD_L, y: PAD_T, w: plotW, h: plotH },
    from,
    to,
  };
}

export function IncidentTimeline({
  timeline,
  laneLabels,
  fromLabel,
  toLabel,
}: {
  timeline: TimelineSignal[] | undefined;
  /** Localized lane names (the component carries no i18n dependency itself). */
  laneLabels: Record<Lane, string>;
  fromLabel: string;
  toLabel: string;
}) {
  const m = buildIncidentTimeline(timeline);
  if (!m) return null;
  return (
    <svg
      className="tsr-timeline"
      viewBox={`0 0 ${m.w} ${m.h}`}
      role="img"
      aria-label={`${fromLabel} → ${toLabel}`}
    >
      {m.lanes.map(({ lane, y }) => (
        <g key={lane}>
          {/* The lane is named in text as well as coloured — colour is never the only cue. */}
          <text className={`tsr-tl-lane-label ${lane}`} x={PAD_L - 8} y={y + 3} textAnchor="end">
            {laneLabels[lane]}
          </text>
          <line
            className="tsr-tl-lane"
            x1={PAD_L}
            y1={y}
            x2={m.plot.x + m.plot.w}
            y2={y}
          />
        </g>
      ))}
      {m.signals.map((s, i) => (
        <circle
          key={`${s.lane}-${s.at}-${i}`}
          className={`tsr-tl-dot ${s.lane} ${s.tone}`}
          cx={s.x.toFixed(1)}
          cy={s.y}
          r={s.r}
        >
          <title>{signalLabel(s)}</title>
        </circle>
      ))}
      <text className="tsr-tl-axis" x={PAD_L} y={m.h - 4}>
        {fromLabel}
      </text>
      <text className="tsr-tl-axis" x={m.plot.x + m.plot.w} y={m.h - 4} textAnchor="end">
        {toLabel}
      </text>
    </svg>
  );
}
