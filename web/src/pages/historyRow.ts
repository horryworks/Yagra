// SPDX-License-Identifier: AGPL-3.0-only
// What one row of Alerts ▸ History says when it is opened (ADR-270): the outage it was grouped
// into, the nodes an outage held, and the notifications that one transition sent.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md). `HistoryRowDetail.tsx` draws it.

import type { AlertHistoryQuery, AlertHistoryRow, DeliveryEvent, DeliveryQuery, DeliveryRow } from '../types/api';
import { LIVENESS_METRIC as LIVENESS } from '../lib/format';

/** The two views of the History screen: the transitions, and (for those who may read it) the
 *  delivery log, which lived on a screen of its own until ADR-270. */
export const HISTORY_VIEWS = ['transitions', 'deliveries'] as const;
export type HistoryView = (typeof HISTORY_VIEWS)[number];

/** The URL key the view is held under. */
export const HISTORY_VIEW_KEY = 'view';

/** What a row records: a fire, a clear, or an alert that had paged being grouped into an outage. */
export const HISTORY_ROW_PHASES = ['fired', 'cleared', 'rolledUp'] as const;
export type HistoryPhase = (typeof HISTORY_ROW_PHASES)[number];

export function phaseOf(r: Pick<AlertHistoryRow, 'resolved' | 'rolled_up'>): HistoryPhase {
  if (r.resolved) return 'cleared';
  return r.rolled_up ? 'rolledUp' : 'fired';
}

/** When a row's transition happened. A roll-up carries the time of the fire it closes in
 *  `at_unix_ms`, so its own moment is when it was written; every other row is its `at_unix_ms`. */
export function momentOf(r: Pick<AlertHistoryRow, 'at_unix_ms' | 'rolled_up' | 'recorded_at'>): number {
  if (r.rolled_up) {
    const written = Date.parse(r.recorded_at);
    if (Number.isFinite(written)) return written;
  }
  return r.at_unix_ms;
}

/** The moment the same check next changed, among the rows on screen, or `null` when none of them
 *  is later. */
export function nextTransitionOf(
  r: Pick<AlertHistoryRow, 'id' | 'check' | 'at_unix_ms' | 'rolled_up' | 'recorded_at'>,
  rows: readonly Pick<AlertHistoryRow, 'id' | 'check' | 'at_unix_ms' | 'rolled_up' | 'recorded_at'>[],
): number | null {
  const at = momentOf(r);
  let next: number | null = null;
  for (const o of rows) {
    if (o.id === r.id || o.check !== r.check) continue;
    const m = momentOf(o);
    if (m > at && (next === null || m < next)) next = m;
  }
  return next;
}

/** The node whose outage this up/down row was grouped into, or `null`. A down node's other alerts
 *  carry their own node there (ADR-087), which is no grouping. */
export function groupedUnderOf(
  r: Pick<AlertHistoryRow, 'metric' | 'root_cause' | 'node'>,
): string | null {
  if (r.metric !== LIVENESS || !r.root_cause || r.root_cause === r.node) return null;
  return r.root_cause;
}

/** Whether this row opened an outage that may hold other nodes: an up/down fire of a node that was
 *  itself grouped under nobody. Only these ask the server for members. */
export function mayHoldGrouped(
  r: Pick<AlertHistoryRow, 'metric' | 'root_cause' | 'node' | 'resolved' | 'rolled_up'>,
): boolean {
  return r.metric === LIVENESS && r.node != null && phaseOf(r) === 'fired' && groupedUnderOf(r) === null;
}

/** How many member rows one opened outage asks for. An outage larger than this is listed in part,
 *  and the list says so (`membersCut`). */
export const GROUPED_MEMBERS_LIMIT = 1000;

/** The query for the nodes an outage held: the rows grouped under the one that opened it. */
export function groupedQueryOf(r: Pick<AlertHistoryRow, 'id'>): AlertHistoryQuery {
  return { outage_row: r.id, limit: GROUPED_MEMBERS_LIMIT };
}

/** The distinct nodes among an outage's member rows, in the order the server gave them. */
export function memberNodes(rows: readonly Pick<AlertHistoryRow, 'node'>[]): string[] {
  const seen = new Set<string>();
  for (const r of rows) if (r.node) seen.add(r.node);
  return [...seen];
}

/** Whether the server stopped at the limit, so the outage may hold more nodes than are listed. */
export function membersCut(rows: readonly unknown[]): boolean {
  return rows.length >= GROUPED_MEMBERS_LIMIT;
}

/** Whether opening this row can show anything: the outage it was grouped into, the nodes it may
 *  hold, or (for someone who may read the delivery log) what it sent. A row with none of these does
 *  not open. */
export function hasDetail(
  r: Pick<AlertHistoryRow, 'metric' | 'root_cause' | 'node' | 'resolved' | 'rolled_up'>,
  canDeliveries: boolean,
): boolean {
  return groupedUnderOf(r) !== null || mayHoldGrouped(r) || (canDeliveries && r.node != null);
}

/** The deliveries one transition can have produced: a fire and the updates that followed it, a
 *  resolve, or the close a roll-up sends. */
export function deliveryEventsOf(r: Pick<AlertHistoryRow, 'resolved' | 'rolled_up'>): DeliveryEvent[] {
  switch (phaseOf(r)) {
    case 'fired':
      return ['fire', 'update'];
    case 'cleared':
      return ['resolve'];
    case 'rolledUp':
      return ['suppress'];
  }
}

/** How far before a transition's own time (`at_unix_ms`) a delivery of it may be stamped: the two
 *  clocks are the poller's and core's, so either may come first by a little. */
export const DELIVERY_LEAD_MS = 60_000;

type TransitionRow = Pick<
  AlertHistoryRow,
  'id' | 'node' | 'check' | 'at_unix_ms' | 'resolved' | 'rolled_up' | 'recorded_at'
>;

/**
 * The delivery-log query for one transition's notifications, or `null` for a row that is not about
 * a node (those are stored under another subject and not shown here). It asks from just before the
 * transition to the same check's next transition on screen, so the server's limit is spent on this
 * transition's deliveries and not on a later outage's. Without a later row on screen it is open
 * ended, which is right for an alert still open.
 */
export function deliveryQueryOf(r: TransitionRow, rows: readonly TransitionRow[]): DeliveryQuery | null {
  if (!r.node) return null;
  const next = nextTransitionOf(r, rows);
  return {
    subject: r.node,
    check_id: r.check,
    event: deliveryEventsOf(r).join(','),
    since: new Date(momentOf(r) - DELIVERY_LEAD_MS).toISOString(),
    ...(next === null ? {} : { until: new Date(next).toISOString() }),
    limit: 100,
  };
}

/**
 * The deliveries that belong to this transition, oldest first: those sent before the same check's
 * next transition, when the screen holds it. The query already asks for that window; this holds
 * the answer to it when the rows on screen have moved on since.
 */
export function deliveriesOf(
  r: TransitionRow,
  rows: readonly TransitionRow[],
  deliveries: readonly DeliveryRow[],
): DeliveryRow[] {
  const until = nextTransitionOf(r, rows) ?? Number.POSITIVE_INFINITY;
  return deliveries
    .filter((d) => Date.parse(d.at) < until)
    .sort((a, b) => Date.parse(a.at) - Date.parse(b.at));
}
