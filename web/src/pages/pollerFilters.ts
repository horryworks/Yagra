// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ Pollers table shows.
//
// Client-side: the poller inventory is bounded by how many pollers were deployed, not by fleet size
// — a 50k-node deployment still has a handful (ui-conventions, "scale-aware lists"). In a `.ts` so
// a test can reach it (testing.md).

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import type { PollerInfo } from '../types/api';

/** The two states a poller reports. A UI-owned union: the backend serializes `status` as a bare
 *  string (`"online"` when it is beating within the offline window, else `"offline"`), so there is
 *  no schema enum to pin this to — which is exactly why it is an `as const` array here rather than
 *  two string literals typed into a dropdown. */
export const POLLER_STATUSES = ['online', 'offline'] as const;
export type PollerStatusFilter = (typeof POLLER_STATUSES)[number];

/**
 * The Settings ▸ Pollers filter row, keyed by `Column.key` (ADR-053 Inc.5).
 *
 * The toolbar's single search box read the id, the pool **and** the version at once, so "0.2" found
 * a poller running v0.2.4 and a poller in a pool called `site-0.2` and could not tell them apart.
 * Each is now its own column — and the version one is the reason this screen wanted a filter row at
 * all: "which boxes are still on the old build" is asked during every rollout (ADR-051) and was
 * previously answerable only by reading the column.
 *
 * `pools` comes from the pool summaries the page already loaded rather than from the rows, because
 * a pool with no live poller still exists and an operator filtering for it should get the honest
 * empty answer rather than an option that is missing.
 */
export function pollerFilters(
  t: TFunction,
  pools: readonly string[],
): Record<string, ColumnFilterSpec<PollerInfo>> {
  return {
    poller: {
      kind: 'text',
      modes: ['contains', 'regex'],
      readText: (p) => [p.id],
      containsSemantics: 'substring',
      placeholder: t('pollers.cols.poller'),
    },
    pool: {
      kind: 'enum',
      options: pools.map((p) => ({ value: p, label: p })),
      readValue: (p) => p.pool,
      allLabel: t('pollers.filter.allPools'),
      counts: 'client',
    },
    status: {
      kind: 'enum',
      options: POLLER_STATUSES.map((s) => ({ value: s, label: t(`pollers.status.${s}`) })),
      readValue: (p) => p.status,
      allLabel: t('pollers.filter.allStatuses'),
      counts: 'client',
    },
    version: {
      kind: 'text',
      modes: ['contains', 'regex'],
      // `null` while a poller has never reported — an empty string rather than the em dash the
      // cell renders, so a term never matches on punctuation the operator did not type.
      readText: (p) => [p.version ?? ''],
      containsSemantics: 'substring',
      placeholder: t('pollers.cols.version'),
    },
  };
}
