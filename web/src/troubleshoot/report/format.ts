// SPDX-License-Identifier: AGPL-3.0-only
// Pure derivation helpers shared by the report bodies. Everything here is a pure function so it is
// unit-testable under vitest's `environment: 'node'`.
//
// **Why bodies derive their own labels:** the backend's `when_label` / `duration` are *pre-rendered
// English* strings (`format!("{cycles} cycles")`, `rel_label(...)`, `"horizontal · 4496"`). They
// bypass `t()` and would render English under JA. So every body formats from the structured `detail`
// via these helpers + `t()`, and treats `when_label`/`duration` as a **fallback only** (a row written
// by an older core, or a malformed detail).

import type { AnalysisFinding } from '../../types/api';
import type { SummaryStat } from './types';

/**
 * Mirrors the backend's `MAX_FINDINGS` (analysis.rs). A job that produced exactly this many findings
 * was truncated, so "total" is a floor, not a count — reports render `60+` rather than lying.
 */
export const MAX_FINDINGS = 60;

/** Narrow a finding's severity defensively (anything unknown reads as `info`). */
export function sevOf(f: AnalysisFinding): 'crit' | 'warn' | 'info' {
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

/** 1–5 sensitivity slider → σ threshold (looser = higher σ). Mirrors the launch drawer. */
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
