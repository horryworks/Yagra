// SPDX-License-Identifier: AGPL-3.0-only
// The shared read of `/fleet/watch` (ADR-227): the Active alerts chips and the shell's
// "nothing is polled" line both ask it, and one poll serves both (`lib/sharedPoll.ts`).

import { createSharedPoll } from '../lib/sharedPoll';
import { api } from '../services/api';
import type { FleetWatch } from '../types/api';

const usePoll = createSharedPoll(() => api.getFleetWatch());

/** The counts of nodes not being watched, or `null` until the first answer (or when refused —
 *  skeleton mode answers 503, and nothing is drawn from no answer). */
export function useFleetWatch(): FleetWatch | null {
  return usePoll().data;
}
