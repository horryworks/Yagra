// SPDX-License-Identifier: AGPL-3.0-only
// A site's local time and delivery state on the header's sub line (ADR-252): the folder pane and
// the node pane both show it, from this one function.
//
// The server judges delivery hours (ADR-226) and sends instants; this only lays them out. It never
// reads a schedule, so the screen cannot say "delivering" while the notifier holds a page.

import type { TFunction } from 'i18next';
import {
  canonicalTimeZone,
  clockText,
  displayTimeZone,
  wallClock,
  zoneWeekdayText,
} from './timeZone';

/** What the server says about delivery right now (`DeliveryNow` in the API). */
export interface DeliveryNow {
  open: boolean;
  closes_at?: string | null;
  opens_at?: string | null;
}

export interface LocalTimeLine {
  /** `HH:MM` and the zone it is read in; null when no folder above names a zone (decision 4). */
  clock: { time: string; zone: string } | null;
  /** The delivery state; null when no schedule reaches the place, or when the server's answer has
   *  run past its own boundary (`stale`) and must not be shown as current. */
  delivery: { open: boolean; text: string } | null;
  /** The server's answer is out of date: the hours it said would close (or open) already did. */
  stale: boolean;
}

/** `HH:MM` at `at` in `tz`, with the weekday in front when it is not the same day as `now` there. */
function boundaryText(at: number, now: number, tz: string, locale: string): string {
  const a = wallClock(at, tz);
  const n = wallClock(now, tz);
  const sameDay = a.year === n.year && a.month === n.month && a.day === n.day;
  const time = clockText(at, tz);
  return sameDay ? time : `${zoneWeekdayText(at, tz, locale)} ${time}`;
}

export function localTimeLine(
  zone: string | null | undefined,
  delivery: DeliveryNow | null | undefined,
  now: number,
  t: TFunction,
  locale: string,
): LocalTimeLine {
  // A name this browser does not know shows no clock rather than throwing on every redraw.
  const known = zone ? canonicalTimeZone(zone) : null;
  const clock = known && zone ? { time: clockText(now, known), zone } : null;
  if (!delivery) return { clock, delivery: null, stale: false };
  const raw = delivery.open ? delivery.closes_at : delivery.opens_at;
  const boundary = raw ? Date.parse(raw) : null;
  if (boundary !== null && !Number.isNaN(boundary) && now >= boundary) {
    return { clock, delivery: null, stale: true };
  }
  // The boundary is read in the site's zone; with none, in the zone the rest of the screen uses.
  const tz = known ?? displayTimeZone();
  const at =
    boundary === null || Number.isNaN(boundary) ? null : boundaryText(boundary, now, tz, locale);
  const text = delivery.open
    ? at
      ? t('localTime.deliveringUntil', { time: at })
      : t('localTime.delivering')
    : at
      ? t('localTime.heldUntil', { time: at })
      : t('localTime.held');
  return { clock, delivery: { open: delivery.open, text }, stale: false };
}
