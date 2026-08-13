// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for all three Reports page filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ReportDefinition, ReportRun, ReportSchedule } from '../types/api';
import {
  DEFAULT_DEFINITION_FILTERS,
  DEFAULT_SAVED_RUN_FILTERS,
  DEFAULT_SCHEDULE_LIST_FILTERS,
  isDefinitionFiltered,
  isSavedRunFiltered,
  isScheduleListFiltered,
  matchesDefinition,
  matchesReportSchedule,
  matchesSavedRun,
  RUN_STATE_FILTERS,
  type DefinitionFilters,
  type SavedRunFilters,
  type ScheduleListFilters,
} from './reportListFilters';

const def = (over: Partial<ReportDefinition> = {}): ReportDefinition => ({
  id: 'd1',
  name: 'Monthly capacity',
  description: 'Interfaces trending towards saturation',
  spec: { sections: [] },
  created_ms: 0,
  updated_ms: 0,
  updated_by: 'admin',
  ...over,
});

const sched = (over: Partial<ReportSchedule> = {}): ReportSchedule => ({
  id: 's1',
  definition_id: 'd1',
  definition_name: 'Monthly capacity',
  frequency: 'monthly',
  at_hour: 6,
  at_minute: 0,
  day_of_week: 0,
  day_of_month: 1,
  enabled: true,
  last_run_ms: null,
  last_status: null,
  next_run_ms: 0,
  ...over,
});

const df = (over: Partial<DefinitionFilters>): DefinitionFilters => ({
  ...DEFAULT_DEFINITION_FILTERS,
  ...over,
});
const sf = (over: Partial<ScheduleListFilters>): ScheduleListFilters => ({
  ...DEFAULT_SCHEDULE_LIST_FILTERS,
  ...over,
});

describe('matchesDefinition', () => {
  it('shows everything when nothing is typed', () => {
    expect(matchesDefinition(def(), DEFAULT_DEFINITION_FILTERS)).toBe(true);
  });

  it('searches the name and the description', () => {
    expect(matchesDefinition(def(), df({ q: 'CAPACITY' }))).toBe(true);
    expect(matchesDefinition(def(), df({ q: 'saturation' }))).toBe(true);
    expect(matchesDefinition(def(), df({ q: 'inventory' }))).toBe(false);
  });

  it('survives a template with no description', () => {
    expect(matchesDefinition(def({ description: null }), df({ q: 'capacity' }))).toBe(true);
    expect(matchesDefinition(def({ description: null }), df({ q: 'saturation' }))).toBe(false);
  });

  it('flips isFiltered for its one field', () => {
    expect(isDefinitionFiltered(DEFAULT_DEFINITION_FILTERS)).toBe(false);
    expect(isDefinitionFiltered(df({ q: 'x' }))).toBe(true);
  });
});

describe('matchesReportSchedule', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesReportSchedule(sched(), DEFAULT_SCHEDULE_LIST_FILTERS)).toBe(true);
  });

  it('filters by enabled', () => {
    expect(matchesReportSchedule(sched({ enabled: false }), sf({ enabled: 'disabled' }))).toBe(true);
    expect(matchesReportSchedule(sched({ enabled: false }), sf({ enabled: 'enabled' }))).toBe(false);
  });

  it('searches the report it renders — the only human handle a schedule has', () => {
    expect(matchesReportSchedule(sched(), sf({ q: 'MONTHLY' }))).toBe(true);
    expect(matchesReportSchedule(sched(), sf({ q: 'weekly' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isScheduleListFiltered(DEFAULT_SCHEDULE_LIST_FILTERS)).toBe(false);
    expect(isScheduleListFiltered(sf({ enabled: 'disabled' }))).toBe(true);
    expect(isScheduleListFiltered(sf({ q: 'x' }))).toBe(true);
  });
});

const run = (over: Partial<ReportRun> = {}): ReportRun => ({
  id: 'r1',
  definition_id: 'd1',
  name: 'Monthly capacity',
  trigger: 'scheduled',
  state: 'succeeded',
  pct: 100,
  error: null,
  range_from_ms: 0,
  range_to_ms: 0,
  section_count: 3,
  created_by: null,
  created_ms: 0,
  started_ms: 0,
  finished_ms: 0,
  ...over,
});

const rf = (over: Partial<SavedRunFilters>): SavedRunFilters => ({
  ...DEFAULT_SAVED_RUN_FILTERS,
  ...over,
});

describe('the saved-run state vocabulary', () => {
  it('offers every state a run can be written in, and not the one nothing writes', () => {
    // `unknown` is what a token this build cannot read degrades to. Offering it would hand the
    // operator a filter that always finds nothing; the API refuses it for the same reason.
    expect(RUN_STATE_FILTERS).toEqual(['queued', 'running', 'succeeded', 'failed']);
    expect(RUN_STATE_FILTERS).not.toContain('unknown');
  });

  it('is the report vocabulary, which is not the analysis one', () => {
    // A finished report `succeeded`; a finished analysis is `done`. Confusing the two is not
    // hypothetical — it shipped, in `notifyWatch.ts`, and announced every successful analysis as
    // a failure for as long as the notice existed.
    expect(RUN_STATE_FILTERS).toContain('succeeded');
    expect(RUN_STATE_FILTERS as readonly string[]).not.toContain('done');
  });
});

describe('matchesSavedRun', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesSavedRun(run(), DEFAULT_SAVED_RUN_FILTERS)).toBe(true);
  });

  it('filters by definition and by state independently', () => {
    expect(matchesSavedRun(run(), rf({ definitionId: 'd1' }))).toBe(true);
    expect(matchesSavedRun(run(), rf({ definitionId: 'd2' }))).toBe(false);
    expect(matchesSavedRun(run(), rf({ state: 'succeeded' }))).toBe(true);
    expect(matchesSavedRun(run(), rf({ state: 'failed' }))).toBe(false);
    expect(matchesSavedRun(run({ state: 'failed' }), rf({ state: 'failed' }))).toBe(true);
  });

  it('keeps an older run of a since-renamed report when the picker selects it', () => {
    // A run carries the name the definition had at the time. Filtering by id rather than by name
    // is what stops a rename orphaning its own history.
    const older = run({ name: 'Capacity (old name)' });
    expect(matchesSavedRun(older, rf({ definitionId: 'd1' }))).toBe(true);
  });

  it('survives a run whose definition has been deleted', () => {
    // The row stays, with `definition_id` null. It must not match a filter naming some other
    // definition, and must not crash one.
    const orphan = run({ definition_id: null });
    expect(matchesSavedRun(orphan, DEFAULT_SAVED_RUN_FILTERS)).toBe(true);
    expect(matchesSavedRun(orphan, rf({ definitionId: 'd1' }))).toBe(false);
  });

  it('searches the report name the run was generated under', () => {
    expect(matchesSavedRun(run(), rf({ q: 'MONTHLY' }))).toBe(true);
    expect(matchesSavedRun(run(), rf({ q: 'weekly' }))).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isSavedRunFiltered(DEFAULT_SAVED_RUN_FILTERS)).toBe(false);
    for (const x of [rf({ definitionId: 'd1' }), rf({ state: 'failed' }), rf({ q: 'x' })]) {
      expect(isSavedRunFiltered(x)).toBe(true);
    }
  });
});
