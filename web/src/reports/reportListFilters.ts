// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Reports page's Templates and Schedules tabs show.
//
// Client-side: both lists are bounded by what an admin authored, not by fleet size
// (ui-conventions). The Saved-reports tab is the fleet-scaling one and belongs in the query — it is
// increment 6, not this. In a `.ts` so a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, matchesEnabled, textMatch } from '../lib/filterQuery';
import type { EnabledState } from '../lib/filterQuery';
import type { ReportDefinition, ReportSchedule } from '../types/api';

export interface DefinitionFilters {
  /** Free text over the template's name and description. */
  q: string;
}

export const DEFAULT_DEFINITION_FILTERS: DefinitionFilters = { q: '' };

export function matchesDefinition(d: ReportDefinition, f: DefinitionFilters): boolean {
  return textMatch(f.q, d.name, d.description);
}

export function isDefinitionFiltered(f: DefinitionFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_DEFINITION_FILTERS);
}

export interface ScheduleListFilters {
  enabled: EnabledState | '';
  /** Free text over the report the schedule renders. */
  q: string;
}

export const DEFAULT_SCHEDULE_LIST_FILTERS: ScheduleListFilters = { enabled: '', q: '' };

/** Whether one report schedule survives the filter.
 *
 *  `definition_name` is what the row shows and the only human-readable handle a schedule has — it
 *  has no name of its own, it *is* "this report, on this cadence". */
export function matchesReportSchedule(s: ReportSchedule, f: ScheduleListFilters): boolean {
  if (!matchesEnabled(f.enabled, s.enabled)) return false;
  return textMatch(f.q, s.definition_name);
}

export function isScheduleListFiltered(f: ScheduleListFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_SCHEDULE_LIST_FILTERS);
}
