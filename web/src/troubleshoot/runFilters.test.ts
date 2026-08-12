// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Troubleshoot ▸ Runs filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { AnalysisJob } from '../types/api';
import {
  DEFAULT_RUN_FILTERS,
  isRunFiltered,
  matchesRun,
  RUN_STATES,
  type RunFilters,
} from './runFilters';

const job = (over: Partial<AnalysisJob> = {}): AnalysisJob => ({
  id: 'j1',
  tool: 'anomaly',
  scope_kind: 'group',
  scope_id: 'g1',
  scope_label: 'Tokyo core',
  params: {},
  state: 'done',
  pct: 100,
  phase: null,
  finding_count: 3,
  summary: 'three interfaces drifting',
  error: null,
  created_ms: 0,
  started_ms: 0,
  finished_ms: 0,
  ...over,
});

const f = (over: Partial<RunFilters>): RunFilters => ({ ...DEFAULT_RUN_FILTERS, ...over });

describe('the run-state vocabulary', () => {
  it('is the same five the API validates against', () => {
    // `api/analysis.rs::JOB_STATES` is the other half. Both sides carry the state as a bare string,
    // so this list *is* the vocabulary — a state missing here is one an operator cannot select.
    expect(RUN_STATES).toEqual(['queued', 'running', 'done', 'failed', 'cancelled']);
  });
});

describe('matchesRun', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesRun(job(), DEFAULT_RUN_FILTERS)).toBe(true);
  });

  it('filters by tool and by state independently', () => {
    expect(matchesRun(job(), f({ tool: 'anomaly' }))).toBe(true);
    expect(matchesRun(job(), f({ tool: 'capacity' }))).toBe(false);
    expect(matchesRun(job(), f({ state: 'done' }))).toBe(true);
    expect(matchesRun(job(), f({ state: 'failed' }))).toBe(false);
    expect(matchesRun(job({ state: 'failed' }), f({ state: 'failed' }))).toBe(true);
  });

  it('searches the scope label, the tool and the summary', () => {
    expect(matchesRun(job(), f({ q: 'TOKYO' }))).toBe(true);
    expect(matchesRun(job(), f({ q: 'anomaly' }))).toBe(true);
    expect(matchesRun(job(), f({ q: 'drifting' }))).toBe(true);
    expect(matchesRun(job(), f({ q: 'osaka' }))).toBe(false);
  });

  it('survives a run that has not produced a summary yet', () => {
    // A queued or running job has none, and a filter must not turn that into a crash.
    const queued = job({ state: 'queued', summary: null });
    expect(matchesRun(queued, f({ q: 'tokyo' }))).toBe(true);
    expect(matchesRun(queued, f({ q: 'drifting' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isRunFiltered(DEFAULT_RUN_FILTERS)).toBe(false);
    for (const x of [f({ tool: 'capacity' }), f({ state: 'failed' }), f({ q: 'x' })]) {
      expect(isRunFiltered(x)).toBe(true);
    }
  });
});
