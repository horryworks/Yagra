// SPDX-License-Identifier: AGPL-3.0-only
// What a quiet alert list is not seeing (ADR-227): how many of its rows are silenced, how many
// nodes are in maintenance, and how many are not being watched at all. Pure, so the counting and
// the "nothing is polled" judgement are testable without a browser.

import type { Alert, FleetWatch } from '../types/api';

/** A row is silenced when it is held under a root cause (outage grouping, ADR-236). It stays in the
 *  list with its own mark; this only counts it. A node in maintenance raises no alert at all, so
 *  it is counted by the `maintenance` chip instead (ADR-234). */
export function isSilenced(a: Pick<Alert, 'root_cause'>): boolean {
  return a.root_cause !== null && a.root_cause !== undefined;
}

export type WatchChipKey = 'silenced' | 'maintenance' | 'notWatched';

export interface WatchChip {
  key: WatchChipKey;
  count: number;
  /** Where the chip leads. `silenced` has none: it narrows the list it sits above. */
  to?: string;
}

/** The chips to draw, in their fixed order, leaving out every one that is zero — an all-zero
 *  answer draws nothing, which is the screen as it was before this existed. */
export function watchChips(input: {
  silenced: number;
  maintenance: number;
  watch: FleetWatch | null;
}): WatchChip[] {
  const chips: WatchChip[] = [];
  if (input.silenced > 0) chips.push({ key: 'silenced', count: input.silenced });
  if (input.maintenance > 0) {
    chips.push({ key: 'maintenance', count: input.maintenance, to: '/alerts/maintenance' });
  }
  const w = input.watch;
  const notWatched = w ? w.no_live_poller + w.no_recent_data : 0;
  if (w && notWatched > 0) {
    chips.push({
      key: 'notWatched',
      count: notWatched,
      // The pool reason is the one an operator can act on in one place; without it the list of
      // silent nodes is where to start.
      to: w.no_live_poller > 0 ? '/settings/pollers' : '/nodes?state=unknown',
    });
  }
  return chips;
}

/** Nothing the caller can see is being polled: every pool-polled node sits in a pool with no live
 *  poller. Never true on a core that cannot see poller liveness (an HA standby), where the pool
 *  count is unknown rather than zero. */
export function nothingPolled(w: FleetWatch | null): boolean {
  return !!w && w.pollers_known && w.pool_polled > 0 && w.no_live_poller === w.pool_polled;
}
