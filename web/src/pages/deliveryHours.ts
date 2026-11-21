// SPDX-License-Identifier: AGPL-3.0-only
// Delivery hours (ADR-226): the day and hour words a routing rule's window (ADR-262) and an old
// schedule are written with, and the "held until" mark on an active alert. Kept out of the `.tsx` so Vitest can reach it.

import type { TFunction } from 'i18next';
import type { DeliverySchedule, HeldNotification } from '../types/api';
import { weekdayName as cadenceWeekday } from '../lib/cadence';

/** ISO weekdays in display order, Monday first. */
export const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7] as const;
/** The days a new window starts with. */
export const WORKDAYS: readonly number[] = [1, 2, 3, 4, 5];

/** `510` → `"08:30"`. */
export function minutesToTime(m: number): string {
  const h = Math.floor(m / 60);
  const mm = m % 60;
  return `${String(h).padStart(2, '0')}:${String(mm).padStart(2, '0')}`;
}

/** `"08:30"` → `510`, or `null` for anything that is not a time of day. */
export function timeToMinutes(s: string): number | null {
  const m = /^(\d{1,2}):(\d{2})$/.exec(s.trim());
  if (!m) return null;
  const h = Number(m[1]);
  const mm = Number(m[2]);
  if (h > 23 || mm > 59) return null;
  return h * 60 + mm;
}

/** An ISO weekday's name (`1` is Monday), through the reports' weekday names so the two agree. */
export function weekdayName(t: TFunction, day: number): string {
  return cadenceWeekday(t, day % 7);
}

/** The table's "days" cell: the listed days in week order, or a run of three or more as a range. */
export function daysText(t: TFunction, days: readonly number[]): string {
  const sorted = WEEKDAYS.filter((d) => days.includes(d));
  if (sorted.length === 0) return '';
  const consecutive = sorted.every((d, i) => i === 0 || d === sorted[i - 1] + 1);
  if (consecutive && sorted.length > 2) {
    return `${weekdayName(t, sorted[0])}–${weekdayName(t, sorted[sorted.length - 1])}`;
  }
  return sorted.map((d) => weekdayName(t, d)).join(', ');
}

/** The table's "hours" cell. An equal pair is the whole day, said as `00:00–24:00`. */
export function hoursText(s: Pick<DeliverySchedule, 'start_minute' | 'end_minute'>): string {
  if (s.start_minute === s.end_minute) return '00:00–24:00';
  return `${minutesToTime(s.start_minute)}–${minutesToTime(s.end_minute)}`;
}

/** The held fire for an active alert's row, or `null`. Matched by node and check, the way the
 *  release loop matches it back; a held resolve says nothing about an open alert. */
export function heldFireFor(
  alert: { node?: string | null; check: string },
  held: readonly HeldNotification[],
  nodeId: string | null,
): HeldNotification | null {
  if (!nodeId) return null;
  return (
    held.find((h) => h.event === 'fire' && h.node_id === nodeId && h.check === alert.check) ?? null
  );
}
