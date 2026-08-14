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
// **ADR-053 Inc.6 moved the controls into a `FilterBar`** — the run list is a CSS grid of rows with
// no header row, so there is nothing to hang a filter row under (decision E). Tool and state became
// sets, and the free text gained NOT and regex.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import {
  specColumns,
  TEXT_MODES,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../lib/columnFilter';
import { ANALYSIS_JOB_STATES, type AnalysisJob, type AnalysisJobState } from '../types/api';
import { TOOLS } from './data';
import type { TFunction } from 'i18next';

/** The states an operator may pick — every one the writers produce, i.e. everything but `unknown`.
 *
 *  A deliberate **subset** of `ANALYSIS_JOB_STATES`, derived from it rather than written out, so a
 *  new state cannot ship missing from the control (the `monitorKinds.ts` pattern). `unknown` is
 *  excluded because nothing writes it — it is what a token this build cannot read degrades to, and
 *  the API refuses it as a filter for the same reason. */
export const RUN_STATES = ANALYSIS_JOB_STATES.filter(
  (s): s is Exclude<AnalysisJobState, 'unknown'> => s !== 'unknown',
);
export type RunState = (typeof RUN_STATES)[number];

/**
 * The screen's filter columns, keyed by the name each control carries in the bar.
 *
 * `q` reads `scope_label` rather than `scope_id`: the label is what the row shows and what an
 * operator knows, and the id is a UUID nobody types. `tool` is read raw off the job — a run from a
 * newer core naming a tool this build lacks simply matches no option, which is the same degradation
 * the row's own rendering does.
 */
export function runFilters(t: TFunction): Record<string, ColumnFilterSpec<AnalysisJob>> {
  return {
    tool: {
      kind: 'enum',
      // From the same catalog the launcher offers, so a new analysis appears here with no second
      // list to remember.
      options: TOOLS.map((tool) => ({ value: tool.id, label: t(tool.name) })),
      readValue: (j) => j.tool,
      allLabel: t('schedule.filter.allTools'),
      counts: 'client',
    },
    state: {
      kind: 'enum',
      options: RUN_STATES.map((s) => ({ value: s, label: t(`runs.state.${s}`) })),
      readValue: (j) => j.state,
      allLabel: t('runs.filter.allStates'),
      counts: 'client',
    },
    q: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (j) => [j.scope_label, j.tool, j.summary],
      containsSemantics: 'substring',
      placeholder: t('runs.filter.searchPlaceholder'),
    },
  };
}

export function runColumns(t: TFunction): FilterableColumn<AnalysisJob>[] {
  return specColumns(runFilters(t));
}

/** Plain-text names for the bar and the mobile sheet — there is no header above each control. */
export function runFilterLabels(t: TFunction): Record<string, string> {
  return {
    tool: t('runs.cols.tool'),
    state: t('runs.cols.state'),
    q: t('runs.cols.scope'),
  };
}
