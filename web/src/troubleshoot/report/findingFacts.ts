// SPDX-License-Identifier: AGPL-3.0-only
// What each analysis writes into a finding's `detail`, read once and named once.
//
// **This module is the TypeScript half of a mirror nothing else guards.** The `detail` object is
// built in Rust (`crates/yagra-core/src/analysis/*.rs`) as free-form JSON, so a renamed key there is
// not a compile error here — it is a body that silently renders `0`, `∞`, or an empty bar. Before
// this file the key strings sat in fifteen `.tsx` files that Vitest never loads
// (`include: ['src/**/*.test.ts']`), so there was no place a test could name them at all.
//
// **Names are analysis-prefixed on purpose.** Four bodies had a `baseOf` and a `ratioOf` and they
// were not the same function: Event storm's baseline is `baseline_mean` (a count), Traffic
// anomaly's is `baseline_mean_bytes`, and Saturation's ratio is a stored field rather than a
// computed quotient. A flat `baseOf` here would have merged two different questions
// (`extensibility.md` §5 — a name must match what the thing contains).
//
// What is genuinely shared is the *arithmetic*, not the fields: see [`ratioOver`].

import { detailNum, detailStr } from './format';
import type { AnalysisFinding } from '../../types/api';
import type { TimelineSignal } from './IncidentTimeline';

/**
 * Peak over baseline, with no baseline meaning "unboundedly above it".
 *
 * Event storm and Traffic anomaly each had their own copy of this, byte-identical apart from the
 * two field names it read (`extensibility.md` §3 — if two functions differ only by a value, pass
 * the value). `Infinity` rather than `0` is what makes the row sort to the top and the meter fill:
 * a node that was silent and is now shouting is the most anomalous case, not the least.
 */
export function ratioOver(peak: number, baseline: number): number {
  return baseline > 0 ? peak / baseline : Infinity;
}

// ── Capacity ────────────────────────────────────────────────────────────────────────────────────

/** Days until the resource is exhausted. `Infinity` = the trend never reaches the ceiling, which is
 *  the healthy answer and must not read as "0 days left". */
export const capacityTteDays = (f: AnalysisFinding) => detailNum(f, 'tte_days') ?? Infinity;
/** The value observed now, in the metric's own unit. */
export const capacityCurrent = (f: AnalysisFinding) => detailNum(f, 'current') ?? 0;
/** Fitted slope per day. Negative means the resource is being released. */
export const capacitySlopePerDay = (f: AnalysisFinding) => detailNum(f, 'slope_per_day') ?? 0;

// ── Correlation ─────────────────────────────────────────────────────────────────────────────────

/** Pearson r in −1..1. */
export const correlationR = (f: AnalysisFinding) => detailNum(f, 'r') ?? 0;

/** The two metrics a correlation is between.
 *
 *  The pair arrives as one `metric` string joined by ` ↔ `, so this is a parse rather than a read.
 *  A row written before the join existed (or by a future core using another separator) yields the
 *  whole string as the left half and an empty right half — visibly odd, rather than an exception in
 *  a report body. */
export function correlationPair(f: AnalysisFinding): [string, string] {
  const parts = f.metric.split(' ↔ ');
  return [parts[0] ?? f.metric, parts[1] ?? ''];
}

/** `r` with an explicit sign, so +0.91 and −0.91 never read alike at a glance. */
export const correlationText = (r: number) => `r = ${r >= 0 ? '+' : ''}${r.toFixed(2)}`;

// ── Event flap ──────────────────────────────────────────────────────────────────────────────────

export const eventFlapCycles = (f: AnalysisFinding) => detailNum(f, 'cycles') ?? 0;
export const eventFlapFires = (f: AnalysisFinding) => detailNum(f, 'fires') ?? 0;
export const eventFlapClears = (f: AnalysisFinding) => detailNum(f, 'clears') ?? 0;

/** Flaps per hour. **Shared with the interface-flap body** — both analyses write `per_hour` and
 *  both had their own one-line reader of it. */
export const perHour = (f: AnalysisFinding) => detailNum(f, 'per_hour') ?? 0;

// ── Event storm ─────────────────────────────────────────────────────────────────────────────────

/** Events in the busiest bucket. */
export const stormPeak = (f: AnalysisFinding) => detailNum(f, 'peak') ?? 0;
/** Mean events per bucket over the baseline window. */
export const stormBaseline = (f: AnalysisFinding) => detailNum(f, 'baseline_mean') ?? 0;
export const stormRatio = (f: AnalysisFinding) => ratioOver(stormPeak(f), stormBaseline(f));

// ── Interface flap ──────────────────────────────────────────────────────────────────────────────

export const flapCount = (f: AnalysisFinding) => detailNum(f, 'flaps') ?? 0;

// ── Incident correlate ──────────────────────────────────────────────────────────────────────────

/**
 * The signals that make up an incident, defensively narrowed.
 *
 * `detail.timeline` is an array of objects composed in Rust, so anything that is not an object with
 * a numeric `at` is dropped rather than reaching the plot — a signal with no timestamp has no
 * position on a timeline, and one bad element must not take the card with it.
 */
export function incidentTimeline(f: AnalysisFinding): TimelineSignal[] {
  const raw = (f.detail as { timeline?: unknown } | null | undefined)?.timeline;
  if (!Array.isArray(raw)) return [];
  return raw.filter(
    (s): s is TimelineSignal =>
      typeof s === 'object' && s !== null && typeof (s as TimelineSignal).at === 'number',
  );
}

/** The distinct signal kinds present — the filter axis ("metric-led or flow-led?"). */
export const incidentKinds = (f: AnalysisFinding) =>
  new Set(incidentTimeline(f).map((s) => s.kind));

/** When the incident began. `0` for an incident with no usable signals, which sorts it last. */
export const incidentEarliest = (f: AnalysisFinding) => {
  const ts = incidentTimeline(f).map((s) => s.at);
  return ts.length ? Math.min(...ts) : 0;
};

// ── Saturation ──────────────────────────────────────────────────────────────────────────────────

/** The conversation's share of the node's traffic, 0..1. **Stored, not computed** — unlike
 *  [`stormRatio`] and [`trafficRatio`], which are quotients. */
export const saturationRatio = (f: AnalysisFinding) => detailNum(f, 'ratio') ?? 0;
export const saturationConversationBytes = (f: AnalysisFinding) =>
  detailNum(f, 'conversation_bytes') ?? 0;
export const saturationNodeBytes = (f: AnalysisFinding) => detailNum(f, 'node_bytes') ?? 0;
/** The interface's rate, or `undefined` when the link speed is unknown — the caller renders the
 *  difference, so this one deliberately does not default to `0`. */
export const saturationInterfaceBps = (f: AnalysisFinding) => detailNum(f, 'interface_bps');

// ── Severity shift ──────────────────────────────────────────────────────────────────────────────

export const shiftRecentFrac = (f: AnalysisFinding) => detailNum(f, 'recent_high_frac') ?? 0;
export const shiftBaselineFrac = (f: AnalysisFinding) => detailNum(f, 'baseline_high_frac') ?? 0;
/** The shift in **percentage points**, not percent — the two fractions are already shares. */
export const shiftDeltaPp = (f: AnalysisFinding) =>
  (shiftRecentFrac(f) - shiftBaselineFrac(f)) * 100;
export const shiftVolume = (f: AnalysisFinding) => detailNum(f, 'recent_total') ?? 0;

// ── Talker shift ────────────────────────────────────────────────────────────────────────────────

export const talkerAddr = (f: AnalysisFinding) => detailStr(f, 'addr');
export const talkerBytes = (f: AnalysisFinding) => detailNum(f, 'bytes') ?? 0;
/** Rank among the node's talkers, 1 = busiest. `99` for a row with no rank, so it sorts last and
 *  tones as `info` rather than as the top talker. */
export const talkerRank = (f: AnalysisFinding) => detailNum(f, 'rank') ?? 99;

// ── Traffic anomaly ─────────────────────────────────────────────────────────────────────────────

export const trafficPeakBytes = (f: AnalysisFinding) => detailNum(f, 'peak_bytes') ?? 0;
export const trafficBaselineBytes = (f: AnalysisFinding) =>
  detailNum(f, 'baseline_mean_bytes') ?? 0;
export const trafficRatio = (f: AnalysisFinding) =>
  ratioOver(trafficPeakBytes(f), trafficBaselineBytes(f));
