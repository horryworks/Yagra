// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Troubleshoot ▸ Scheduled table shows.
//
// Client-side: the schedule list is bounded by what an operator set up, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { enumOptions } from '../lib/filterPresets';
import { ENABLED_STATES } from '../lib/filterQuery';
import { TOOLS } from './data';
import { ANALYSIS_SCHEDULE_STATUSES, type AnalysisSchedule } from '../types/api';

/**
 * The Troubleshoot ▸ Scheduled filter row, keyed by `Column.key` (ADR-053 Inc.3).
 *
 * ⚠️ The Analysis column filters on the **catalog label**, not on the raw `tool` token, because that
 * is what the cell renders — a schedule for a tool this build does not know shows its raw key, and
 * typing what is on screen has to find it either way. The options come from the same catalog the
 * schedule form offers, so a new analysis appears here with no second list to remember.
 *
 * `scope_label` rather than `scope_id`: the label is what the row shows and what the operator knows;
 * the id is a UUID nobody types.
 */
export function scheduleFilters(t: TFunction): Record<string, ColumnFilterSpec<AnalysisSchedule>> {
  return {
    tool: {
      kind: 'enum',
      options: TOOLS.map((tool) => ({ value: tool.id, label: t(tool.name) })),
      readValue: (s) => s.tool,
      allLabel: t('schedule.cols.analysis'),
      counts: 'client',
    },
    scope: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (s) => [s.scope_label],
      containsSemantics: 'substring',
      placeholder: t('schedule.cols.scope'),
    },
    next: {
      kind: 'enum',
      options: enumOptions(ENABLED_STATES, t, 'common:filter.'),
      // The Next-run cell says "Paused" for a disabled schedule rather than a time, so the enabled
      // flag is what that column is really about.
      readValue: (s) => (s.enabled ? 'enabled' : 'disabled'),
      allLabel: t('common:filter.allEnabled'),
      counts: 'client',
    },
    last: {
      kind: 'enum',
      // Same source as the badge's `Record`, so the filter and the cell cannot disagree about what
      // an outcome is called — `busy` in particular must not read as a failure in either.
      options: enumOptions(ANALYSIS_SCHEDULE_STATUSES, t, 'schedule.status.'),
      // A schedule that has never run shows an em dash and has no status to select.
      readValue: (s) => s.last_status ?? null,
      allLabel: t('schedule.cols.last'),
      counts: 'client',
    },
  };
}
