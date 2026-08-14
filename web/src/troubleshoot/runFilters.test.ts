// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Troubleshoot ▸ Runs filter (no DOM — Vitest node env).
//
// Rewritten for ADR-053 Inc.6, which put this screen on the shared column-filter model. Every
// property the hand-written `matchesRun` was tested for is still asserted; the additions are the
// multi-value and NOT cases the single-valued dropdown could not express.

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import type { AnalysisJob } from '../types/api';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { runColumns, runFilterLabels, RUN_STATES } from './runFilters';

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

const t = ((k: string) => k) as unknown as TFunction;
const COLS = runColumns(t);
const DEFAULTS = defaultFilters(COLS);
const f = (over: FilterState): FilterState => ({ ...DEFAULTS, ...over });
const matches = (j: AnalysisJob, state: FilterState) => buildPredicate(COLS, state, 0)(j);

describe('the run-state vocabulary', () => {
  it('is the same five the API validates against', () => {
    // `api/analysis.rs::JOB_STATES` is the other half. Both sides carry the state as a bare string,
    // so this list *is* the vocabulary — a state missing here is one an operator cannot select.
    expect(RUN_STATES).toEqual(['queued', 'running', 'done', 'failed', 'cancelled']);
  });

  it('offers every one of them as an option', () => {
    const state = COLS.find((c) => c.key === 'state')?.filter;
    expect(state?.kind).toBe('enum');
    if (state?.kind !== 'enum') return;
    expect(state.options.map((o) => o.value)).toEqual([...RUN_STATES]);
  });
});

describe('the column set', () => {
  it('labels every column, so the bar never shows a raw key', () => {
    const labels = runFilterLabels(t);
    for (const c of COLS) expect(labels[c.key]).toBeTruthy();
  });
});

describe('the predicate', () => {
  it('shows everything when nothing is set', () => {
    expect(matches(job(), DEFAULTS)).toBe(true);
  });

  it('filters by tool and by state independently', () => {
    expect(matches(job(), f({ tool: 'anomaly' }))).toBe(true);
    expect(matches(job(), f({ tool: 'capacity' }))).toBe(false);
    expect(matches(job(), f({ state: 'done' }))).toBe(true);
    expect(matches(job(), f({ state: 'failed' }))).toBe(false);
    expect(matches(job({ state: 'failed' }), f({ state: 'failed' }))).toBe(true);
  });

  it('takes several states at once — "everything that did not finish"', () => {
    const set = f({ state: 'failed,cancelled' });
    expect(matches(job({ state: 'failed' }), set)).toBe(true);
    expect(matches(job({ state: 'cancelled' }), set)).toBe(true);
    expect(matches(job({ state: 'done' }), set)).toBe(false);
  });

  it('searches the scope label, the tool and the summary', () => {
    expect(matches(job(), f({ q: 'TOKYO' }))).toBe(true);
    expect(matches(job(), f({ q: 'anomaly' }))).toBe(true);
    expect(matches(job(), f({ q: 'drifting' }))).toBe(true);
    expect(matches(job(), f({ q: 'osaka' }))).toBe(false);
  });

  it('excludes with NOT', () => {
    expect(matches(job(), f({ q: '!tokyo' }))).toBe(false);
    expect(matches(job(), f({ q: '!osaka' }))).toBe(true);
  });

  it('survives a run that has not produced a summary yet', () => {
    // A queued or running job has none, and a filter must not turn that into a crash.
    const queued = job({ state: 'queued', summary: null });
    expect(matches(queued, f({ q: 'tokyo' }))).toBe(true);
    expect(matches(queued, f({ q: 'drifting' }))).toBe(false);
  });

  it('flips isAnyFiltered for every column', () => {
    expect(isAnyFiltered(COLS, DEFAULTS)).toBe(false);
    for (const x of [f({ tool: 'capacity' }), f({ state: 'failed' }), f({ q: 'x' })]) {
      expect(isAnyFiltered(COLS, x)).toBe(true);
    }
  });
});
