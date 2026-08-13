// SPDX-License-Identifier: AGPL-3.0-only
// Which rows each of the Reports page's three tabs shows.
//
// Client-side, all three. Templates and Schedules are bounded by what an admin authored rather
// than by fleet size (ui-conventions), which is the straightforward case.
//
// ⚠️ **Saved reports is the one that needs its reason written down.** `GET /api/v1/reports/runs`
// does take `definition_id`, `state` and `since` — that filter is real and API clients use it —
// but this tab is fed by an SSE store: `useReportRunsStore` is seeded once and then live-patched as
// runs progress. Fetching a filtered seed while the stream keeps upserting unfiltered frames would
// put rows back that the filter had removed, one progress tick later. Troubleshoot ▸ Runs filters
// client-side for exactly this reason (`troubleshoot/runFilters.ts`), and the two screens are
// twins — answering the same question two different ways would be the worse outcome.
//
// In a `.ts` so a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, matchesEnabled, textMatch } from '../lib/filterQuery';
import type { EnabledState } from '../lib/filterQuery';
import {
  REPORT_RUN_STATES,
  type ReportDefinition,
  type ReportRun,
  type ReportRunState,
  type ReportSchedule,
} from '../types/api';

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

/** The run states an operator may pick — everything the writers produce, i.e. all but `unknown`.
 *
 *  Derived from `REPORT_RUN_STATES` rather than written out, so a new state cannot ship missing
 *  from the dropdown. `unknown` is excluded because nothing writes it: it is what a token this
 *  build cannot read degrades to, and the API refuses it as a filter for the same reason. */
export const RUN_STATE_FILTERS = REPORT_RUN_STATES.filter(
  (s): s is Exclude<ReportRunState, 'unknown'> => s !== 'unknown',
);

export interface SavedRunFilters {
  /** Only runs of this report definition (`''` ⇒ any). */
  definitionId: string;
  state: Exclude<ReportRunState, 'unknown'> | '';
  /** Free text over the report's name. */
  q: string;
}

export const DEFAULT_SAVED_RUN_FILTERS: SavedRunFilters = { definitionId: '', state: '', q: '' };

/** Whether one saved run survives the filter.
 *
 *  A run keeps the *name* the definition had when it ran, and the definition id beside it. Both
 *  are matched on purpose: the name is what the row shows and what an operator searches for, and
 *  the id is what the picker selects — including for a definition since renamed, whose older runs
 *  carry the old name and would otherwise look like someone else's. */
export function matchesSavedRun(r: ReportRun, f: SavedRunFilters): boolean {
  if (f.definitionId && r.definition_id !== f.definitionId) return false;
  if (f.state && r.state !== f.state) return false;
  return textMatch(f.q, r.name);
}

export function isSavedRunFiltered(f: SavedRunFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_SAVED_RUN_FILTERS);
}
