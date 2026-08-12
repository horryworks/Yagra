// SPDX-License-Identifier: AGPL-3.0-only
// Which runs Troubleshoot ▸ Runs shows.
//
// ⚠️ **Client-side, and the API's own filter is deliberately not used here.** `GET
// /api/v1/analysis/jobs` takes `tool`, `state` and `since` — that filter is real and API clients
// use it — but this screen is fed by an SSE store: `useTroubleshootStream` seeds it once and then
// live-patches rows as runs progress. Fetching a filtered seed while the stream keeps upserting
// unfiltered frames would put rows back that the filter had removed, one progress tick later.
// The whole set is already in the browser, which is the same reason Alerts ▸ Active filters here
// (see `pages/activeAlertFilters.ts`).
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import { isFiltered as isFilteredAgainst, textMatch } from '../lib/filterQuery';
import type { AnalysisJob, AnalysisToolKey } from '../types/api';

/** Every state a run can be in, as the API validates them. Matches `JOB_STATES` in
 *  `api/analysis.rs`; the column is a bare string on both sides, so this list is the vocabulary. */
export const RUN_STATES = ['queued', 'running', 'done', 'failed', 'cancelled'] as const;
export type RunState = (typeof RUN_STATES)[number];

export interface RunFilters {
  tool: AnalysisToolKey | '';
  state: RunState | '';
  /** Free text over what the run was scoped to. */
  q: string;
}

export const DEFAULT_RUN_FILTERS: RunFilters = { tool: '', state: '', q: '' };

/** Whether one run survives the filter.
 *
 *  `scope_label` rather than `scope_id`: the label is what the row shows and what an operator
 *  knows, and the id is a UUID nobody types. */
export function matchesRun(j: AnalysisJob, f: RunFilters): boolean {
  if (f.tool && j.tool !== f.tool) return false;
  if (f.state && j.state !== f.state) return false;
  return textMatch(f.q, j.scope_label, j.tool, j.summary);
}

export function isRunFiltered(f: RunFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_RUN_FILTERS);
}
