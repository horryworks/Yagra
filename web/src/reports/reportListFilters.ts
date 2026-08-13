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

// **⚠️ These three are the reason `useClientFilters` takes a `url` option.** The column key IS the
// URL key (ADR-053 decision 12 refuses a prefix so old bookmarks keep working), and all three
// tables live on `/reports` — Templates and Schedules both have a `name` column, Saved reports and
// Schedules both have `next`/`when`. URL-backed they would write to each other's keys and filter
// the wrong table. So these three keep their state in the component, and the ADR records why.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { clientRangePresets, enumOptions } from '../lib/filterPresets';
import { ENABLED_STATES } from '../lib/filterQuery';
import {
  REPORT_RUN_STATES,
  REPORT_TRIGGERS,
  type ReportDefinition,
  type ReportRun,
  type ReportRunState,
  type ReportSchedule,
} from '../types/api';

/** Templates: name and description, which is what the search box read. */
export function definitionFilters(
  t: TFunction,
): Record<string, ColumnFilterSpec<ReportDefinition>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (d) => [d.name, d.description],
      containsSemantics: 'substring',
      placeholder: t('defs.cols.template'),
    },
    updated: {
      kind: 'range',
      presets: clientRangePresets(t),
      defaultPreset: 'all',
      readTime: (d) => d.updated_ms,
    },
  };
}

/** Report schedules.
 *
 *  `definition_name` is what the row shows and the only human-readable handle a schedule has — it
 *  has no name of its own, it *is* "this report, on this cadence". */
export function reportScheduleFilters(
  t: TFunction,
): Record<string, ColumnFilterSpec<ReportSchedule>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (s) => [s.definition_name],
      containsSemantics: 'substring',
      placeholder: t('scheds.cols.report'),
    },
    next: {
      kind: 'range',
      presets: clientRangePresets(t),
      defaultPreset: 'all',
      readTime: (s) => s.next_run_ms,
    },
    enabled: {
      kind: 'enum',
      options: enumOptions(ENABLED_STATES, t, 'common:filter.'),
      readValue: (s) => (s.enabled ? 'enabled' : 'disabled'),
      allLabel: t('common:filter.allEnabled'),
      counts: 'client',
    },
  };
}

/** The run states an operator may pick — everything the writers produce, i.e. all but `unknown`.
 *
 *  Derived from `REPORT_RUN_STATES` rather than written out, so a new state cannot ship missing
 *  from the dropdown. `unknown` is excluded because nothing writes it: it is what a token this
 *  build cannot read degrades to, and the API refuses it as a filter for the same reason. */
export const RUN_STATE_FILTERS = REPORT_RUN_STATES.filter(
  (s): s is Exclude<ReportRunState, 'unknown'> => s !== 'unknown',
);

/**
 * Saved reports.
 *
 * ⚠️ **The report picker became a text condition, and that is a real change in what can be asked.**
 * The toolbar offered a dropdown of definition *ids*, which caught the runs of a report that had
 * since been renamed — a run keeps the name it had when it ran. A column filter cannot offer that
 * list: the Report column renders the name, so its options would be ids displayed as today's names,
 * and selecting one would silently drop the older rows it claims to include. Matching the name the
 * row actually shows is honest about what it does; finding a renamed report's history now means
 * searching for either name, which is a thing an operator can see and do.
 */
export function savedRunFilters(t: TFunction): Record<string, ColumnFilterSpec<ReportRun>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('runs.cols.report'),
    },
    status: {
      kind: 'enum',
      options: RUN_STATE_FILTERS.map((s) => ({ value: s, label: t(`runs.state.${s}`) })),
      readValue: (r) => r.state,
      allLabel: t('runs.cols.status'),
      counts: 'client',
    },
    trigger: {
      kind: 'enum',
      options: enumOptions(REPORT_TRIGGERS, t, 'trigger.'),
      readValue: (r) => r.trigger,
      allLabel: t('runs.cols.trigger'),
      counts: 'client',
    },
    when: {
      kind: 'range',
      presets: clientRangePresets(t),
      defaultPreset: 'all',
      readTime: (r) => r.created_ms,
    },
  };
}
