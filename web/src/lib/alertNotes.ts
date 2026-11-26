// SPDX-License-Identifier: AGPL-3.0-only
// What an active alert's row says beside the alert itself — the Notes column (ADR-243 decision 5).
//
// Six sources, and only the last two are not on the alert: the root cause (ADR-236's outage grouping),
// why an SNMP GET came back empty (ADR-268), the flapping flag, the "waiting for a neighbour" hold
// (ADR-236 Inc.2), and the delivery-hours hold (ADR-226), which comes from
// `GET /api/v1/held-notifications` and is matched by the caller, and how many nodes an outage's
// border holds (ADR-270), counted by the caller from the other alerts (`groupedCounts`). Both the
// Active alerts table and the dashboard widget draw from this list, so the two cannot disagree about
// which notes a row carries or the order they come in.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import type { SnmpFailure } from '../types/api';
import { rootCause, type HasSubject } from './alertSubject';
import { LIVENESS_METRIC as LIVENESS } from './format';

/** One note, in the order a row draws them. */
export type AlertNote =
  | { kind: 'upstream'; nodeId: string }
  | { kind: 'self' }
  | { kind: 'grouping'; count: number }
  | { kind: 'snmpFailure'; reason: SnmpFailure }
  | { kind: 'flapping' }
  | { kind: 'waitingNeighbour' }
  | { kind: 'held'; releaseAt: string | null };

/** Every kind of note, in the order a row draws them — also the Notes column's filter options and
 *  its sort order. The two lines under it fail to compile when this list and `AlertNote` disagree,
 *  in either direction. */
export const NOTE_KINDS = [
  'upstream',
  'self',
  'grouping',
  'snmpFailure',
  'flapping',
  'waitingNeighbour',
  'held',
] as const;
export type NoteKind = (typeof NOTE_KINDS)[number];
type Same<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
export const NOTE_KINDS_MATCH_ALERT_NOTE: Same<NoteKind, AlertNote['kind']> = true;

/** The fields the notes read off an alert. */
export interface NotedAlert extends HasSubject {
  root_cause?: string | null;
  flapping?: boolean;
  notify_held_until?: number | null;
  snmp_failure?: SnmpFailure | null;
}

/** The held notification for this alert, as far as the notes need it, or `null`. */
export interface HeldFire {
  release_at?: string | null;
}

/**
 * The notes for one alert, cause first and holds last.
 *
 * The order is the reading order of the question a row raises: *why* is this alerting (another
 * node's outage, or this node's own, then what the SNMP GET ran into), *is it real* (flapping), and
 * only then *was anyone told* (the two holds).
 */
export function alertNotes(a: NotedAlert, held: HeldFire | null, grouped = 0): AlertNote[] {
  const out: AlertNote[] = [];
  const cause = rootCause(a);
  if (cause.kind === 'upstream') out.push({ kind: 'upstream', nodeId: cause.nodeId });
  if (cause.kind === 'self') out.push({ kind: 'self' });
  if (grouped > 0) out.push({ kind: 'grouping', count: grouped });
  if (a.snmp_failure) out.push({ kind: 'snmpFailure', reason: a.snmp_failure });
  if (a.flapping) out.push({ kind: 'flapping' });
  if (a.notify_held_until != null) out.push({ kind: 'waitingNeighbour' });
  if (held) out.push({ kind: 'held', releaseAt: held.release_at ?? null });
  return out;
}

/**
 * How many down nodes each node's outage holds (ADR-270): the other liveness alerts grouped under
 * it, counted from the alerts on screen. Keyed by node id; a node holding none is absent. What the
 * `grouping` note on an outage's border says.
 */
export function groupedCounts(
  alerts: readonly (NotedAlert & { metric?: string | null })[],
): Map<string, number> {
  const out = new Map<string, number>();
  for (const a of alerts) {
    const cause = rootCause(a);
    if (cause.kind !== 'upstream' || a.metric !== LIVENESS) continue;
    out.set(cause.nodeId, (out.get(cause.nodeId) ?? 0) + 1);
  }
  return out;
}

/** The count to hand [`alertNotes`] for one alert: its outage's members, on its up/down alert only. */
export function groupedFor(
  a: NotedAlert & { metric?: string | null },
  counts: ReadonlyMap<string, number>,
  nodeId: string | null,
): number {
  return a.metric === LIVENESS && nodeId !== null ? (counts.get(nodeId) ?? 0) : 0;
}

/** A stable React key for one note within a row. */
export function noteKey(n: AlertNote): string {
  return n.kind === 'upstream' ? `upstream:${n.nodeId}` : n.kind;
}
