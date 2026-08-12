// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ Pollers table shows.
//
// Client-side: the poller inventory is bounded by how many pollers were deployed, not by fleet size
// — a 50k-node deployment still has a handful (ui-conventions, "scale-aware lists"). In a `.ts` so
// a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, textMatch } from '../lib/filterQuery';
import type { PollerInfo } from '../types/api';

/** The two states a poller reports. A UI-owned union: the backend serializes `status` as a bare
 *  string (`"online"` when it is beating within the offline window, else `"offline"`), so there is
 *  no schema enum to pin this to — which is exactly why it is an `as const` array here rather than
 *  two string literals typed into a dropdown. */
export const POLLER_STATUSES = ['online', 'offline'] as const;
export type PollerStatusFilter = (typeof POLLER_STATUSES)[number];

export interface PollerFilters {
  status: PollerStatusFilter | '';
  /** A pool name, or `''` for every pool. */
  pool: string;
  /** Free text over the poller's id, its pool and its version. */
  q: string;
}

export const DEFAULT_POLLER_FILTERS: PollerFilters = { status: '', pool: '', q: '' };

/** Whether one poller survives the filter.
 *
 *  The version is searched because "which boxes are still on the old build" is a question asked
 *  during every rollout, and the column is otherwise only readable by eye. */
export function matchesPoller(p: PollerInfo, f: PollerFilters): boolean {
  if (f.status && p.status !== f.status) return false;
  if (f.pool && p.pool !== f.pool) return false;
  return textMatch(f.q, p.id, p.pool, p.version);
}

export function isPollerFiltered(f: PollerFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_POLLER_FILTERS);
}
