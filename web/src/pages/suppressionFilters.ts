// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the suppression screen shows — Alerts ▸ Maintenance windows, the one way to silence
// alerts since ADR-234.
//
// Client-side, and permitted to be: the list is bounded by what an operator typed in, not by
// fleet size (`ui-conventions.md`, "scale-aware lists"). Which makes this the only thing deciding
// what an operator sees, so it lives in a `.ts` where a test can reach it (testing.md).
//
// **What is left here is what a column reads off a row, and nothing else.** The hand-written
// `matchesX(row, filters)` predicates are gone (ADR-053 Inc.5): the row test is now
// `lib/filterPredicate.ts::buildPredicate`, shared by every screen. Two things came free with that
// and are worth knowing, because both used to be this file's job: the columns are ANDed, and an
// inactive column never reads its row — which is what keeps `useEntityNames` from being asked to
// resolve a name for every row on screen.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { MAINTENANCE_STATUSES, windowStatus } from './maintenanceStatus';
import type { MaintenanceWindow } from '../types/api';

// ─────────────────────────────────────────────────────────── maintenance windows

/** The statuses the dropdown offers, derived from the badge's own vocabulary so a fifth status
 *  cannot appear in the column and be un-filterable. */
export const MAINTENANCE_STATUS_FILTERS = MAINTENANCE_STATUSES;

// ───────────────────────────────────────────── the filter row (ADR-053 Inc.5)
//
// The screen moved from a toolbar (one search box + one status dropdown) to a filter row under
// the header, which is strictly more expressive: the free-text box searched several fields at once
// and could not say *which*, so "prod" matched a window named prod and a window covering the prod
// group identically. Per-column conditions say which.
//
// ⚠️ `labelOf` resolves an id through `useEntityNames`, whose resolver **enqueues every
// id it is asked about**. `buildPredicate` compiles an inactive column to `null` and never reads a
// row through it, so it is consulted only while a term is typed — the same property the old
// `f.q.trim() === ''` early return bought, now a consequence of the shared predicate rather than
// something each `matchesX` had to remember.

/** The Alerts ▸ Maintenance filter row, keyed by `Column.key`. */
export function windowFilters(
  t: TFunction,
  labelOf: (w: MaintenanceWindow) => string,
  nowMs: number,
): Record<string, ColumnFilterSpec<MaintenanceWindow>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (w) => [w.name],
      containsSemantics: 'substring',
      placeholder: t('maintenance.cols.name'),
    },
    scope: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (w) => [labelOf(w)],
      containsSemantics: 'substring',
      placeholder: t('maintenance.cols.scope'),
    },
    status: {
      kind: 'enum',
      // Read off the badge's own vocabulary, so a fifth status cannot appear in the column and be
      // un-filterable.
      options: MAINTENANCE_STATUS_FILTERS.map((s) => ({
        value: s,
        label: t(`maintenance.status.${s}`),
      })),
      readValue: (w) => windowStatus(w, nowMs).labelKey,
      allLabel: t('maintenance.filter.allStatuses'),
      counts: 'client',
    },
  };
}
