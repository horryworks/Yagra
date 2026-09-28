// SPDX-License-Identifier: AGPL-3.0-only
// Shared per-group health rollup for the site/topology dashboard widgets (site-matrix,
// region-rollup, geo-map). They all need the same server-computed per-group state tally, so this
// dedupes it into one fetch + one 15s poll regardless of how many are mounted — reading
// `/fleet/group-summary` so the numbers cover the WHOLE fleet per group, not the first page of
// `listNodes()` (A-1). The sharing itself is `lib/sharedPoll.ts` (ADR-184).

import { createSharedPoll } from '../lib/sharedPoll';
import { api } from '../services/api';
import type { FleetGroupSummary } from '../types/api';

const usePoll = createSharedPoll(() => api.getFleetGroupSummary());

/** Subscribe to the shared per-group summary (kept fresh on a 15s poll while any widget is mounted). */
export function useGroupSummary(): {
  summary: FleetGroupSummary | null;
  loading: boolean;
  error: boolean;
} {
  const { data, loading, error } = usePoll();
  return { summary: data, loading, error };
}
