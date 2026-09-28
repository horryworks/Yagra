// SPDX-License-Identifier: AGPL-3.0-only
// Shared fleet status summary for the dashboard status widgets (status-summary, health-ring,
// nodes-down KPI). They all need the same server-computed per-state tally, so this dedupes it into
// one fetch + one 15s poll regardless of how many status-widgets are mounted — reading
// `/fleet/summary` so the numbers are correct over the WHOLE fleet, not the first page of
// `listNodes()` (S12). The sharing itself is `lib/sharedPoll.ts` (ADR-184).

import { createSharedPoll } from '../lib/sharedPoll';
import { api } from '../services/api';
import type { FleetSummary } from '../types/api';

const usePoll = createSharedPoll(() => api.getFleetSummary());

/** Subscribe to the shared fleet summary (kept fresh on a 15s poll while any widget is mounted). */
export function useFleetSummary(): { summary: FleetSummary | null; loading: boolean; error: boolean } {
  const { data, loading, error } = usePoll();
  return { summary: data, loading, error };
}
