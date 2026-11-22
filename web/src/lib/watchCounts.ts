// SPDX-License-Identifier: AGPL-3.0-only
// What a quiet alert list is not seeing (ADR-227): how many of its rows are grouped into an outage,
// and how many nodes are sending no data at all. Pure, so the counting and the "nothing is polled"
// judgement are testable without a browser.
//
// Since ADR-227 Inc.2 only the first is a chip, because it is the only one that narrows this list.
// The second is a sentence above it: it counts nodes, not alerts, and leads to another screen.
// Nodes in maintenance are not shown here at all — the Maintenance screen has them, and the server
// leaves them out of `no_recent_data` (decision 8).

import type { Alert, FleetWatch } from '../types/api';

/** A row is silenced when it is held under a root cause (outage grouping, ADR-236). It stays in the
 *  list with its own mark; this only counts it. A node in maintenance raises no alert at all
 *  (ADR-234), so it has no row to count. */
export function isSilenced(a: Pick<Alert, 'root_cause'>): boolean {
  return a.root_cause !== null && a.root_cause !== undefined;
}

export type WatchChipKey = 'silenced';

export interface WatchChip {
  key: WatchChipKey;
  count: number;
}

/** The chips to draw, leaving out every one that is zero — an all-zero answer draws nothing,
 *  which is the screen as it was before this existed. */
export function watchChips(input: { silenced: number }): WatchChip[] {
  return input.silenced > 0 ? [{ key: 'silenced', count: input.silenced }] : [];
}

/** The sentence about nodes sending no data, and where it leads; `null` when there are none. */
export interface NotWatchedLine {
  count: number;
  to: string;
}

export function notWatchedLine(w: FleetWatch | null): NotWatchedLine | null {
  if (!w) return null;
  const count = w.no_live_poller + w.no_recent_data;
  if (count === 0) return null;
  // The pool reason is the one an operator can act on in one place; without it the list of
  // silent nodes is where to start.
  return { count, to: w.no_live_poller > 0 ? '/settings/pollers' : '/nodes?state=unknown' };
}

/** Nothing the caller can see is being polled: every pool-polled node sits in a pool with no live
 *  poller. Never true on a core that cannot see poller liveness (an HA standby), where the pool
 *  count is unknown rather than zero. */
export function nothingPolled(w: FleetWatch | null): boolean {
  return !!w && w.pollers_known && w.pool_polled > 0 && w.no_live_poller === w.pool_polled;
}
