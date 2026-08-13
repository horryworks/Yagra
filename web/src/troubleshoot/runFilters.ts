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
import { ANALYSIS_JOB_STATES, type AnalysisJob, type AnalysisJobState, type AnalysisToolKey } from '../types/api';

/** The states an operator may pick — every one the writers produce, i.e. everything but `unknown`.
 *
 *  A deliberate **subset** of `ANALYSIS_JOB_STATES`, derived from it rather than written out, so a
 *  new state cannot ship missing from the dropdown (the `monitorKinds.ts` pattern). `unknown` is
 *  excluded because nothing writes it — it is what a token this build cannot read degrades to, and
 *  the API refuses it as a filter for the same reason. */
export const RUN_STATES = ANALYSIS_JOB_STATES.filter(
  (s): s is Exclude<AnalysisJobState, 'unknown'> => s !== 'unknown',
);
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
