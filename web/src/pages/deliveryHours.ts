// SPDX-License-Identifier: AGPL-3.0-only
// Delivery hours (ADR-226): the judgement behind the schedule dialog, the table's cells and the
// "held until" mark on an active alert. Kept out of the `.tsx` so Vitest can reach it.

import type { TFunction } from 'i18next';
import type { DeliverySchedule, HeldNotification } from '../types/api';
import { weekdayName as cadenceWeekday } from '../lib/cadence';

/** ISO weekdays in display order, Monday first. */
export const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7] as const;
/** The days a new schedule starts with. */
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

/** What the dialog edits. Times are strings because that is what the inputs hold. */
export interface ScheduleDraft {
  name: string;
  days: number[];
  start: string;
  end: string;
  groupIds: string[];
  labels: string[];
}

export function scheduleDraftFrom(s?: DeliverySchedule): ScheduleDraft {
  return {
    name: s?.name ?? '',
    days: s ? [...s.days] : [...WORKDAYS],
    start: minutesToTime(s?.start_minute ?? 8 * 60 + 30),
    end: minutesToTime(s?.end_minute ?? 17 * 60 + 20),
    groupIds: s ? [...s.group_ids] : [],
    labels: s ? [...s.labels] : [],
  };
}

/** Why a draft cannot be saved; each is a key under `deliveryHours.err.`. */
export type DraftError = 'name' | 'days' | 'time' | 'target';

/** The body the API takes, or the first thing wrong with the draft — the same checks the server
 *  makes, so the dialog can say what is wrong before sending. */
export function scheduleBodyFrom(
  d: ScheduleDraft,
  enabled: boolean,
):
  | {
      body: {
        name: string;
        enabled: boolean;
        days: number[];
        start_minute: number;
        end_minute: number;
        group_ids: string[];
        labels: string[];
      };
    }
  | { error: DraftError } {
  const name = d.name.trim();
  if (name === '') return { error: 'name' };
  if (d.days.length === 0) return { error: 'days' };
  const start = timeToMinutes(d.start);
  const end = timeToMinutes(d.end);
  if (start === null || end === null) return { error: 'time' };
  const labels = d.labels.map((l) => l.trim()).filter((l) => l !== '');
  if (d.groupIds.length === 0 && labels.length === 0) return { error: 'target' };
  return {
    body: {
      name,
      enabled,
      days: WEEKDAYS.filter((x) => d.days.includes(x)),
      start_minute: start,
      end_minute: end,
      group_ids: d.groupIds,
      labels,
    },
  };
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
