// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Reports page's Templates and Schedules filters (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ReportDefinition, ReportSchedule } from '../types/api';
import {
  DEFAULT_DEFINITION_FILTERS,
  DEFAULT_SCHEDULE_LIST_FILTERS,
  isDefinitionFiltered,
  isScheduleListFiltered,
  matchesDefinition,
  matchesReportSchedule,
  type DefinitionFilters,
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
