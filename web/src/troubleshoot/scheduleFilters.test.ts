// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Troubleshoot ▸ Scheduled filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { AnalysisSchedule } from '../types/api';
import {
  DEFAULT_SCHEDULE_FILTERS,
  isScheduleFiltered,
  matchesSchedule,
  type ScheduleFilters,
} from './scheduleFilters';

const sched = (over: Partial<AnalysisSchedule> = {}): AnalysisSchedule => ({
  id: 's1',
  tool: 'anomaly',
  scope_kind: 'group',
  scope_id: 'g1',
  scope_label: 'Tokyo core',
  frequency: 'daily',
  at_hour: 3,
  at_minute: 0,
  day_of_week: 0,
  day_of_month: 1,
  enabled: true,
  params: {},
  last_run_ms: null,
  last_status: null,
  next_run_ms: 0,
  ...over,
});

const f = (over: Partial<ScheduleFilters>): ScheduleFilters => ({
  ...DEFAULT_SCHEDULE_FILTERS,
  ...over,
});

describe('matchesSchedule', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesSchedule(sched(), DEFAULT_SCHEDULE_FILTERS)).toBe(true);
  });

  it('filters by tool and by enabled', () => {
    expect(matchesSchedule(sched(), f({ tool: 'anomaly' }))).toBe(true);
    expect(matchesSchedule(sched(), f({ tool: 'flap' }))).toBe(false);
    expect(matchesSchedule(sched({ enabled: false }), f({ enabled: 'disabled' }))).toBe(true);
    expect(matchesSchedule(sched({ enabled: false }), f({ enabled: 'enabled' }))).toBe(false);
  });

  it('searches the scope label, which is what the row shows', () => {
    // The scope id is a UUID nobody types; the label is the handle an operator has.
    expect(matchesSchedule(sched(), f({ q: 'TOKYO' }))).toBe(true);
    expect(matchesSchedule(sched(), f({ q: 'g1' }))).toBe(false);
    expect(matchesSchedule(sched(), f({ q: 'anomaly' }))).toBe(true);
  });

  it('flips isFiltered for every field', () => {
    expect(isScheduleFiltered(DEFAULT_SCHEDULE_FILTERS)).toBe(false);
    for (const x of [f({ tool: 'flap' }), f({ enabled: 'disabled' }), f({ q: 'x' })]) {
      expect(isScheduleFiltered(x)).toBe(true);
    }
  });
});
