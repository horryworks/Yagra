// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Reports page's three filter rows (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ReportDefinition, ReportRun, ReportSchedule } from '../types/api';
import {
  definitionFilters,
  reportScheduleFilters,
  savedRunFilters,
  RUN_STATE_FILTERS,
} from './reportListFilters';
import {
  defaultFilters,
  isAnyFiltered,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { applyFilters, matchesFilters } from '../lib/filterPredicate';

const t = ((k: string) => k) as unknown as Parameters<typeof definitionFilters>[0];
const NOW = Date.parse('2026-08-13T12:00:00Z');
const DAY = 24 * 60 * 60 * 1000;

const cols = <T,>(specs: Record<string, unknown>): FilterableColumn<T>[] =>
  Object.entries(specs).map(([key, filter]) => ({ key, filter }) as FilterableColumn<T>);

const def = (over: Partial<ReportDefinition> = {}): ReportDefinition => ({
  id: 'd1',
  name: 'Monthly capacity',
  description: 'Interfaces trending towards saturation',
  spec: { sections: [] },
  created_ms: 0,
  updated_ms: NOW,
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
  next_run_ms: NOW,
  ...over,
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
  created_ms: NOW,
  started_ms: 0,
  finished_ms: 0,
  ...over,
});

const DEF_COLS = cols<ReportDefinition>(definitionFilters(t));
const SCHED_COLS = cols<ReportSchedule>(reportScheduleFilters(t));
const RUN_COLS = cols<ReportRun>(savedRunFilters(t));

const state = <T,>(c: FilterableColumn<T>[], over: Record<string, string>): FilterState => ({
  ...defaultFilters(c),
  ...over,
});

describe('all three Reports tables', () => {
  it('start unfiltered', () => {
    for (const [c, row] of [
      [DEF_COLS, def()],
      [SCHED_COLS, sched()],
      [RUN_COLS, run()],
    ] as const) {
      const d = defaultFilters(c as FilterableColumn<unknown>[]);
      expect(isAnyFiltered(c as FilterableColumn<unknown>[], d)).toBe(false);
      expect(matchesFilters(row, c as FilterableColumn<unknown>[], d, NOW)).toBe(true);
    }
  });

  it('share column keys, which is exactly why they are not URL-backed', () => {
    // ⚠️ The column key IS the URL key (ADR-053 decision 12 refuses a prefix). All three tables sit
    // on `/reports`, so URL-backing them would have two tables writing `name` — each clobbering the
    // other. This asserts the collision is real, so nobody "fixes" the local state later.
    const keys = (c: FilterableColumn<unknown>[]) => c.map((x) => x.key);
    const shared = keys(DEF_COLS as FilterableColumn<unknown>[]).filter((k) =>
      keys(SCHED_COLS as FilterableColumn<unknown>[]).includes(k),
    );
    expect(shared).toContain('name');
  });
});

describe('the templates filter row', () => {
  it('searches the name and the description, as the search box did', () => {
    expect(matchesFilters(def(), DEF_COLS, state(DEF_COLS, { name: 'MONTHLY' }), NOW)).toBe(true);
    expect(matchesFilters(def(), DEF_COLS, state(DEF_COLS, { name: 'saturation' }), NOW)).toBe(true);
    expect(matchesFilters(def(), DEF_COLS, state(DEF_COLS, { name: 'weekly' }), NOW)).toBe(false);
  });

  it('narrows by when it was last edited', () => {
    const rows = [def({ id: 'a' }), def({ id: 'b', updated_ms: NOW - 40 * DAY })];
    expect(applyFilters(rows, DEF_COLS, state(DEF_COLS, { updated: '7d' }), NOW).map((r) => r.id)).toEqual(
      ['a'],
    );
    expect(applyFilters(rows, DEF_COLS, state(DEF_COLS, { updated: '90d' }), NOW)).toHaveLength(2);
  });
});

describe('the report-schedules filter row', () => {
  it('searches the report the schedule renders — its only human handle', () => {
    expect(matchesFilters(sched(), SCHED_COLS, state(SCHED_COLS, { name: 'capacity' }), NOW)).toBe(
      true,
    );
    expect(matchesFilters(sched(), SCHED_COLS, state(SCHED_COLS, { name: 'uptime' }), NOW)).toBe(
      false,
    );
  });

  it('filters paused schedules on the column that shows the state', () => {
    const rows = [sched({ id: 'a' }), sched({ id: 'b', enabled: false })];
    expect(
      applyFilters(rows, SCHED_COLS, state(SCHED_COLS, { enabled: 'disabled' }), NOW).map((r) => r.id),
    ).toEqual(['b']);
  });
});

describe('the saved-reports filter row', () => {
  it('offers every state the writers produce and no more', () => {
    // Derived from `REPORT_RUN_STATES`, so a new state cannot ship missing from the list.
    expect(RUN_STATE_FILTERS).toEqual(['queued', 'running', 'succeeded', 'failed']);
    // `unknown` is what a token this build cannot read degrades to; nothing writes it, and the API
    // refuses it as a filter for the same reason.
    expect(RUN_STATE_FILTERS).not.toContain('unknown');
    const status = savedRunFilters(t).status;
    expect(status.kind === 'enum' && status.options.map((o) => o.value)).toEqual([
      ...RUN_STATE_FILTERS,
    ]);
  });

  it('matches the report name the run kept, not a definition id', () => {
    // ⚠️ The toolbar's dropdown selected a definition *id*, which caught runs of a report that had
    // since been renamed. A column filter cannot offer that list without labelling ids with today's
    // names, so it matches what the cell renders. A renamed report's history is found by either
    // name — visible and doable, rather than silently included.
    const rows = [
      run({ id: 'a', name: 'Monthly capacity' }),
      run({ id: 'b', name: 'Capacity (old)', definition_id: 'd1' }),
    ];
    expect(applyFilters(rows, RUN_COLS, state(RUN_COLS, { name: 'Monthly' }), NOW).map((r) => r.id)).toEqual(
      ['a'],
    );
    expect(applyFilters(rows, RUN_COLS, state(RUN_COLS, { name: 'capacity' }), NOW)).toHaveLength(2);
  });

  it('filters by state and by trigger separately', () => {
    const rows = [
      run({ id: 'a', state: 'succeeded', trigger: 'scheduled' }),
      run({ id: 'b', state: 'failed', trigger: 'manual' }),
    ];
    expect(applyFilters(rows, RUN_COLS, state(RUN_COLS, { status: 'failed' }), NOW).map((r) => r.id)).toEqual(
      ['b'],
    );
    expect(applyFilters(rows, RUN_COLS, state(RUN_COLS, { trigger: 'manual' }), NOW).map((r) => r.id)).toEqual(
      ['b'],
    );
    // Both at once, which two independent single-choice dropdowns could do but not combine with a
    // time window.
    expect(
      applyFilters(rows, RUN_COLS, state(RUN_COLS, { status: 'failed', when: '24h' }), NOW),
    ).toHaveLength(1);
  });

  it('narrows by when the report was generated', () => {
    const rows = [run({ id: 'a' }), run({ id: 'b', created_ms: NOW - 10 * DAY })];
    expect(applyFilters(rows, RUN_COLS, state(RUN_COLS, { when: '24h' }), NOW).map((r) => r.id)).toEqual(
      ['a'],
    );
    expect(applyFilters(rows, RUN_COLS, state(RUN_COLS, { when: '30d' }), NOW)).toHaveLength(2);
  });
});
