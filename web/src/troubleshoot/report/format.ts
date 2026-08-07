// SPDX-License-Identifier: AGPL-3.0-only
// Pure derivation helpers shared by the report bodies. Everything here is a pure function so it is
// unit-testable under vitest's `environment: 'node'`.
//
// **Why bodies derive their own labels:** the backend's `when_label` / `duration` are *pre-rendered
// English* strings (`format!("{cycles} cycles")`, `rel_label(...)`, `"horizontal · 4496"`). They
// bypass `t()` and would render English under JA. So every body formats from the structured `detail`
// via these helpers + `t()`, and treats `when_label`/`duration` as a **fallback only** (a row written
// by an older core, or a malformed detail).

import type { AnalysisFinding, FindingSeverity } from '../../types/api';
import type { SummaryStat } from './types';

/**
 * Mirrors the backend's `MAX_FINDINGS` (analysis.rs). A job that produced exactly this many findings
 * was truncated, so "total" is a floor, not a count — reports render `60+` rather than lying.
 */
export const MAX_FINDINGS = 60;

/** Narrow a finding's severity defensively (anything unknown reads as `info`).
 *
 *  Takes the field rather than the row so the Saved-findings screen — whose rows are
 *  `SavedFinding`, not `AnalysisFinding` — narrows by the same rule instead of a second copy of
 *  it. A defensive narrowing written twice is two answers to "what does an unknown severity mean". */
export function sevOf(f: { severity: string }): FindingSeverity {
  return f.severity === 'crit' || f.severity === 'warn' ? f.severity : 'info';
}

/** Findings at a given severity. */
export function countSeverity(findings: AnalysisFinding[], sev: 'crit' | 'warn'): number {
  return findings.filter((f) => sevOf(f) === sev).length;
}

/** Distinct nodes represented (findings with no node — e.g. fleet-wide rows — don't count). */
export function countNodes(findings: AnalysisFinding[]): number {
  return new Set(findings.map((f) => f.node_id).filter(Boolean)).size;
}

/** Distinct values of a `detail` string field — for tools whose entity is not the node. */
export function countDetailValues(findings: AnalysisFinding[], key: string): number {
  const seen = new Set<string>();
  for (const f of findings) {
    const v = detailStr(f, key);
    if (v) seen.add(v);
  }
  return seen.size;
}

/** The findings total, rendered `60+` when the backend truncated at [`MAX_FINDINGS`]. */
export function totalLabel(findings: AnalysisFinding[]): string {
  return findings.length >= MAX_FINDINGS ? `${MAX_FINDINGS}+` : String(findings.length);
}

/** A `detail` field as a finite number, or `undefined` when absent/unusable. */
export function detailNum(f: AnalysisFinding, key: string): number | undefined {
  const v = (f.detail as Record<string, unknown> | null | undefined)?.[key];
  return typeof v === 'number' && Number.isFinite(v) ? v : undefined;
}

/** A `detail` field as a non-empty string, or `undefined`. */
export function detailStr(f: AnalysisFinding, key: string): string | undefined {
  const v = (f.detail as Record<string, unknown> | null | undefined)?.[key];
  return typeof v === 'string' && v.length > 0 ? v : undefined;
}

/** The largest value of a numeric `detail` field across findings (`undefined` when none have it). */
export function maxDetail(findings: AnalysisFinding[], key: string): number | undefined {
  let best: number | undefined;
  for (const f of findings) {
    const v = detailNum(f, key);
    if (v !== undefined && (best === undefined || v > best)) best = v;
  }
  return best;
}

/** The sum of a numeric `detail` field across findings. */
export function sumDetail(findings: AnalysisFinding[], key: string): number {
  let total = 0;
  for (const f of findings) total += detailNum(f, key) ?? 0;
  return total;
}

/** 1–5 sensitivity slider → σ threshold (looser = higher σ). The single definition — the launch
 *  drawer imports this one when submitting the job. */
export function sigmaFor(slider: number): number {
  return 4.5 - 0.5 * slider;
}

/** An integer-ish count for display (avoids `12.000000001` from float sums). */
export function fmtCount(n: number): string {
  return Number.isInteger(n) ? String(n) : n.toFixed(1);
}

/**
 * The standard four summary stats (crit / warn / total / nodes). Tools whose entity IS the node use
 * this verbatim; tools whose entity is something else (a source IP, a signature) must NOT — they
 * build their own so the strip counts the right thing.
 */
export function nodeSummaryStats(): SummaryStat[] {
  return [
    {
      labelKey: 'report.common.summary.critical',
      tone: 'crit',
      value: (f) => String(countSeverity(f, 'crit')),
    },
    {
      labelKey: 'report.common.summary.warning',
      tone: 'warn',
      value: (f) => String(countSeverity(f, 'warn')),
    },
    { labelKey: 'report.common.summary.total', value: totalLabel },
    {
      labelKey: 'report.common.summary.nodes',
      separatorBefore: true,
      value: (f) => String(countNodes(f)),
    },
  ];
}

/** Sort modes every report supports; bodies add their own on top. */
export type CommonSort = 'score' | 'node';

/** Sort a findings list by a common mode (returns a new array). */
export function sortCommon(findings: AnalysisFinding[], mode: CommonSort): AnalysisFinding[] {
  const l = findings.slice();
  if (mode === 'score') return l.sort((a, b) => b.score - a.score);
  return l.sort((a, b) => a.node_name.localeCompare(b.node_name));
}

/** Sort by a numeric `detail` field, descending; findings missing it sink to the bottom. */
export function sortByDetail(findings: AnalysisFinding[], key: string): AnalysisFinding[] {
  return findings
    .slice()
    .sort((a, b) => (detailNum(b, key) ?? -Infinity) - (detailNum(a, key) ?? -Infinity));
}

/**
 * A day count as a localizable magnitude + unit key. Returning the unit rather than a formatted
 * string is what lets JA say「4ヶ月」— the Rust `human_days` equivalent builds English inline and
 * cannot go through `t()`.
 */
export function humanDays(days: number): { count: number; unit: 'h' | 'd' | 'mo' } {
  if (!Number.isFinite(days) || days < 0) return { count: 0, unit: 'd' };
  if (days < 1) return { count: Math.round(days * 24), unit: 'h' };
  if (days < 90) return { count: Math.round(days), unit: 'd' };
  return { count: Math.round(days / 30), unit: 'mo' };
}

/**
 * Which way a correlated pair moves. `r >= 0` counts as co-rising — matching the backend's `>= 0.0`
 * exactly, so a perfectly flat pair is bucketed identically on both sides.
 */
export function correlationDirection(r: number): 'coRising' | 'inverse' {
  return r >= 0 ? 'coRising' : 'inverse';
}

/** Urgency bucket for a capacity projection (mirrors the backend's 7/30/90-day score bands). */
export function capacityBucket(tteDays: number): 'soon' | 'mid' | 'far' {
  if (tteDays <= 30) return 'soon';
  if (tteDays <= 90) return 'mid';
  return 'far';
}

/** A node flapping at least once an hour is chronic rather than intermittent. */
export function flapBucket(perHour: number): 'chronic' | 'intermittent' {
  return perHour >= 1 ? 'chronic' : 'intermittent';
}

/**
 * Scan shape from a source's fan-out. Many hosts ⇒ a horizontal sweep (hunting one open service);
 * many ports ⇒ a vertical probe (mapping a few hosts).
 *
 * Mirrors the backend's `distinct_dst >= distinct_ports` **including the tie**, which resolves to
 * horizontal — the backend ships this classification only inside an English `duration` string, so it
 * has to be recomputed here to be localizable, and it must agree exactly.
 */
export function scanPattern(distinctDst: number, distinctPorts: number): 'horizontal' | 'vertical' {
  return distinctDst >= distinctPorts ? 'horizontal' : 'vertical';
}

/**
 * The rule name behind an `event_flap` finding. The backend encodes it as `event:{rule_name}` in
 * `metric`; the `rule_id` in `detail` is a grouping key only and must never be rendered (no raw
 * UUIDs in the UI).
 */
export function eventRuleName(metric: string): string {
  return metric.startsWith('event:') ? metric.slice('event:'.length) : metric;
}

/** One rule's churn rolled up across every node it fired on. */
export interface RuleGroup {
  /** Grouping key — `detail.rule_id` when present, else the rule name. Never displayed. */
  key: string;
  ruleName: string;
  nodes: number;
  fires: number;
  clears: number;
  cycles: number;
  /** The worst per-hour rate seen on any single node for this rule. */
  worstPerHour: number;
  /** The highest finding score in the group (drives severity/ordering). */
  score: number;
}

/**
 * Roll `event_flap` findings up by rule. A flat list answers "which node is flapping"; this answers
 * "which RULE is thrashing across the fleet" — a different and often more actionable question, and
 * one a per-finding list structurally cannot show. Pure, so it is unit-tested directly.
 */
export function groupByRule(findings: AnalysisFinding[]): RuleGroup[] {
  const acc = new Map<string, RuleGroup & { nodeIds: Set<string> }>();
  for (const f of findings) {
    const ruleName = eventRuleName(f.metric);
    const key = detailStr(f, 'rule_id') ?? ruleName;
    let g = acc.get(key);
    if (!g) {
      g = {
        key,
        ruleName,
        nodes: 0,
        fires: 0,
        clears: 0,
        cycles: 0,
        worstPerHour: 0,
        score: 0,
        nodeIds: new Set<string>(),
      };
      acc.set(key, g);
    }
    g.fires += detailNum(f, 'fires') ?? 0;
    g.clears += detailNum(f, 'clears') ?? 0;
    g.cycles += detailNum(f, 'cycles') ?? 0;
    g.worstPerHour = Math.max(g.worstPerHour, detailNum(f, 'per_hour') ?? 0);
    g.score = Math.max(g.score, f.score);
    if (f.node_id) g.nodeIds.add(f.node_id);
  }
  return [...acc.values()]
    .map(({ nodeIds, ...g }) => ({ ...g, nodes: nodeIds.size }))
    .sort((a, b) => b.cycles - a.cycles);
}

/** The signal array of an `incident_correlate` finding, defensively (a non-array detail ⇒ empty). */
function timelineOf(f: AnalysisFinding): { at?: unknown; kind?: unknown }[] {
  const raw = (f.detail as { timeline?: unknown } | null | undefined)?.timeline;
  return Array.isArray(raw) ? (raw as { at?: unknown; kind?: unknown }[]) : [];
}

/** How many signals an incident finding carries — the "how much evidence" number. */
export function timelineLength(f: AnalysisFinding): number {
  return timelineOf(f).length;
}

/** The distinct signal kinds in an incident (metric/event/flow/…), for the mix and CSV. */
export function timelineKinds(f: AnalysisFinding): string[] {
  return [
    ...new Set(
      timelineOf(f)
        .map((s) => (typeof s.kind === 'string' ? s.kind : undefined))
        .filter((k): k is string => Boolean(k)),
    ),
  ];
}

/** Quote a CSV field (RFC 4180): wrap in quotes, double any embedded quote. */
function csvField(v: string): string {
  return `"${v.replace(/"/g, '""')}"`;
}

/**
 * Render findings as CSV text using a descriptor's column spec. Pure (no DOM) so it is testable;
 * the shell wraps it in a Blob download. CRLF per RFC 4180.
 */
export function toCsv(
  columns: { header: string; cell: (f: AnalysisFinding) => string }[],
  findings: AnalysisFinding[],
): string {
  const head = columns.map((c) => csvField(c.header)).join(',');
  const rows = findings.map((f) => columns.map((c) => csvField(c.cell(f))).join(','));
  return [head, ...rows].join('\r\n');
}

/** Severity band for a "how many times normal" ratio, shared by the event-storm and
 *  traffic-anomaly report bodies.
 *
 *  Both had their own byte-identical copy. The thresholds decide which colour an operator sees
 *  against a spike, so two copies is two places a tuning change has to land — and the one that is
 *  missed silently keeps the old bands. */
export function ratioBucket(r: number): 'x10' | 'x3' | 'low' {
  if (r >= 10) return 'x10';
  if (r >= 3) return 'x3';
  return 'low';
}

/** Split the backend's synthetic `kind: 'info'` tier-unavailable rows out of the real findings.
 *
 *  Every report descriptor's contract depends on this: a body is written assuming it never sees a
 *  notice row, so a notice leaking through renders as a finding with no metric and no severity. */
export function splitNotices<T extends { kind: string }>(
  rows: T[],
): { findings: T[]; notices: T[] } {
  return {
    findings: rows.filter((f) => f.kind !== 'info'),
    notices: rows.filter((f) => f.kind === 'info'),
  };
}

/** How many topology neighbours corroborated an incident (ADR-022 Increment 2).
 *
 *  Defensive on purpose, and additive: a finding written before the neighbour expansion has no
 *  `peer_count` key at all and answers 0, which is exactly what it meant. Reported separately from
 *  the signal count because that count now *includes* peers — an incident whose evidence is mostly
 *  a neighbour's reads very differently from one confined to the node, and the card would otherwise
 *  claim more local activity than there was. */
export function peerCountOf(f: { detail?: unknown }): number {
  const raw = (f.detail as { peer_count?: unknown } | null | undefined)?.peer_count;
  return typeof raw === 'number' && Number.isFinite(raw) && raw > 0 ? Math.floor(raw) : 0;
}
