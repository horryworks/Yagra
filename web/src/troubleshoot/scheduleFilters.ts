// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Troubleshoot ▸ Scheduled table shows.
//
// Client-side: the schedule list is bounded by what an operator set up, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, matchesEnabled, textMatch } from '../lib/filterQuery';
import type { EnabledState } from '../lib/filterQuery';
import type { AnalysisSchedule, AnalysisToolKey } from '../types/api';

export interface ScheduleFilters {
  tool: AnalysisToolKey | '';
  enabled: EnabledState | '';
  /** Free text over the tool and what the schedule is scoped to. */
  q: string;
}

export const DEFAULT_SCHEDULE_FILTERS: ScheduleFilters = { tool: '', enabled: '', q: '' };

/** Whether one schedule survives the filter.
 *
 *  `scope_label` is searched rather than `scope_id` because the label is what the row shows and
 *  what the operator knows — the id is a UUID nobody types. */
export function matchesSchedule(s: AnalysisSchedule, f: ScheduleFilters): boolean {
  if (f.tool && s.tool !== f.tool) return false;
  if (!matchesEnabled(f.enabled, s.enabled)) return false;
  return textMatch(f.q, s.tool, s.scope_label);
}

export function isScheduleFiltered(f: ScheduleFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_SCHEDULE_FILTERS);
}
