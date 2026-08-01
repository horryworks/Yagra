// SPDX-License-Identifier: AGPL-3.0-only
// The Troubleshoot catalog's two header statistics.
//
// Extracted from TroubleshootCatalogPage.tsx so both are testable (Vitest never runs `.tsx`). The
// runtime average in particular has a shape worth pinning: `started_ms`/`finished_ms` are nullable
// on the wire, and a job that was cancelled before it started can produce a negative duration that
// would drag the mean below zero.

import type { AnalysisJob } from '../types/api';

const DAY_MS = 86_400_000;

/** Count of jobs created in the last 24h. `now` is injectable for deterministic tests. */
export function runsToday(jobs: AnalysisJob[], now: number = Date.now()): number {
  const since = now - DAY_MS;
  return jobs.filter((j) => j.created_ms >= since).length;
}

/** Mean runtime of finished jobs, formatted `"Nm Ns"` (or `"—"` when none have completed).
 *
 *  Only jobs with both timestamps count, and a negative span is discarded rather than averaged in:
 *  a job cancelled before it started carries `finished < started`, and one of those would pull the
 *  displayed mean below the real one. */
export function avgRuntime(jobs: AnalysisJob[]): string {
  const durs = jobs
    .filter((j) => j.started_ms != null && j.finished_ms != null)
    .map((j) => (j.finished_ms as number) - (j.started_ms as number))
    .filter((d) => d >= 0);
  if (durs.length === 0) return '—';
  const avg = durs.reduce((a, b) => a + b, 0) / durs.length / 1000;
  const m = Math.floor(avg / 60);
  const s = Math.round(avg % 60);
  return m > 0 ? `${m}m ${s}s` : `${s}s`;
}
