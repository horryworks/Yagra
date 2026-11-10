// SPDX-License-Identifier: AGPL-3.0-only
// The one display time zone (ADR-221): every time the WebUI shows, and every wall time an operator
// types, is read in this zone. It is the browser's own unless the operator picked one with the
// top-bar clock, and that choice lives in `prefs.ts` (`timeZone`, synced to the account).
//
// WHY A MODULE VARIABLE AND NOT A STORE READ
// `lib/format.ts` is imported by nearly every screen and by modules `prefs.ts` itself imports, so
// reading the store from here would be an import cycle. `App.tsx` pushes the preference in with
// `setDisplayTimeZone`, and `AppShell` remounts the routed page when it changes, so nothing has to
// subscribe.
//
// Two daylight-saving edges have no single answer when a wall time is turned into an instant. The
// rule is core's (`yagra_base::wall_clock::resolve`), written again here because the screen has to
// predict what core will do: a wall time inside a gap is the instant the gap ends, and one inside
// an overlap is its first occurrence.
//
// ZONE NAMES (ADR-253). The names a person may choose are core's zone database, generated into
// `zoneNames.json` - the same set NetBox offers, aliases such as `Japan` included. A name is kept
// in the spelling it was given: `Intl` is asked to compute a time in it, never to rename it, which
// is what once turned `Asia/Kolkata` into Chromium's `Asia/Calcutta` (ADR-237).

import ZONE_NAMES from './zoneNames.json';

let chosen: string | null = null;

// The browser's zone, read once rather than on every formatted time (it builds a formatter), and
// read again when the tab comes back into view: a laptop that travelled reports its new zone then.
let browserZone: string | null = null;
if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', () => {
    browserZone = null;
  });
}

/** The browser's IANA zone, or `UTC` when the runtime cannot say. */
export function browserTimeZone(): string {
  if (browserZone == null) {
    try {
      browserZone = Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
    } catch {
      browserZone = 'UTC';
    }
  }
  return browserZone;
}

/** Whether this browser can compute a time in `name`. */
function computable(name: string): boolean {
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: name });
    return true;
  } catch {
    return false;
  }
}

let known: { list: string[]; set: Set<string> } | null = null;

/** The zone names core knows that this browser can also compute, read once. */
function knownZones(): { list: string[]; set: Set<string> } {
  if (!known) {
    const list = (ZONE_NAMES as string[]).filter(computable);
    known = { list, set: new Set(list) };
  }
  return known;
}

/** Whether `name` is a zone a person may choose: core knows it and this browser can compute it.
 *  Matched as written, as NetBox matches it. */
export function isKnownTimeZone(name: string): boolean {
  return !!name && knownZones().set.has(name);
}

/** `name` unchanged when it is a zone this screen can show, else `null` (ADR-253 decisions 1, 5).
 *  Blanks around it are dropped; nothing else about the spelling changes. */
export function checkedZone(name: string | null | undefined): string | null {
  const s = name?.trim() ?? '';
  return isKnownTimeZone(s) ? s : null;
}

/** Set the operator's choice; `null` (or a name this runtime does not know) follows the browser. */
export function setDisplayTimeZone(name: string | null): void {
  chosen = name && isKnownTimeZone(name) ? name : null;
}

/** The zone every time is shown in. */
export function displayTimeZone(): string {
  return chosen ?? browserTimeZone();
}

/** Whether the operator picked a zone rather than following the browser. */
export function isZoneChosen(): boolean {
  return chosen != null;
}

/** Every zone a person may choose, for the picker: core's zone database (the set NetBox offers,
 *  aliases included), less any name this browser cannot compute. Sorted; `UTC` is in it. */
export function knownTimeZones(): string[] {
  return [...knownZones().list];
}

const partsCache = new Map<string, Intl.DateTimeFormat>();

function partsFormatter(tz: string): Intl.DateTimeFormat {
  let f = partsCache.get(tz);
  if (!f) {
    f = new Intl.DateTimeFormat('en-US', {
      timeZone: tz,
      hourCycle: 'h23',
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      weekday: 'short',
    });
    partsCache.set(tz, f);
  }
  return f;
}

/** What a clock in `tz` reads at `ms`. `month` is 1-12, `weekday` 0=Sun … 6=Sat. */
export interface WallClock {
  year: number;
  month: number;
  day: number;
  hour: number;
  minute: number;
  second: number;
  weekday: number;
}

const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'];

export function wallClock(ms: number, tz: string = displayTimeZone()): WallClock {
  const get = (parts: Intl.DateTimeFormatPart[], type: string) =>
    parts.find((p) => p.type === type)?.value ?? '0';
  const parts = partsFormatter(tz).formatToParts(new Date(ms));
  return {
    year: Number(get(parts, 'year')),
    month: Number(get(parts, 'month')),
    day: Number(get(parts, 'day')),
    hour: Number(get(parts, 'hour')) % 24,
    minute: Number(get(parts, 'minute')),
    second: Number(get(parts, 'second')),
    weekday: Math.max(0, WEEKDAYS.indexOf(get(parts, 'weekday'))),
  };
}

/** A wall clock as if it were UTC, in ms — the arithmetic both directions below share. */
function asUtc(
  w: Pick<WallClock, 'year' | 'month' | 'day' | 'hour' | 'minute' | 'second'>,
): number {
  return Date.UTC(w.year, w.month - 1, w.day, w.hour, w.minute, w.second);
}

/** The instants a wall time names in `tz`: none (a gap), one, or two (an overlap), earliest first. */
function instantsOf(wall: number, tz: string): number[] {
  const offsetAt = (t: number) => asUtc(wallClock(t, tz)) - Math.floor(t / 1000) * 1000;
  const offsets = new Set([
    offsetAt(wall - 86_400_000),
    offsetAt(wall),
    offsetAt(wall + 86_400_000),
  ]);
  return [...offsets]
    .map((o) => wall - o)
    .filter((t) => asUtc(wallClock(t, tz)) === wall)
    .sort((a, b) => a - b);
}

/** The instant (ms) that `year-month-day hour:minute` means in `tz`, resolving a daylight-saving
 *  gap to its end and an overlap to its first occurrence (core's rule). */
export function zonedWallToMs(
  year: number,
  month: number,
  day: number,
  hour: number,
  minute: number,
  tz: string = displayTimeZone(),
): number {
  const wall = Date.UTC(year, month - 1, day, hour, minute, 0);
  for (let m = 0; m <= 24 * 60; m++) {
    const found = instantsOf(wall + m * 60_000, tz);
    if (found.length > 0) return found[0];
  }
  return wall;
}

/** The short name of `tz` at `ms` — `JST`, `EDT`, `UTC`, or `GMT+5:30` where no abbreviation is
 *  in common use. In English on purpose: it is a code, not a word, and an operator compares it
 *  with what their devices print. */
const abbreviationCache = new Map<string, Intl.DateTimeFormat>();

export function zoneAbbreviation(tz: string = displayTimeZone(), ms: number = Date.now()): string {
  try {
    let f = abbreviationCache.get(tz);
    if (!f) {
      f = new Intl.DateTimeFormat('en-US', { timeZone: tz, timeZoneName: 'short' });
      abbreviationCache.set(tz, f);
    }
    const part = f
      .formatToParts(new Date(ms))
      .find((p) => p.type === 'timeZoneName')?.value;
    return part ?? tz;
  } catch {
    return tz;
  }
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** `HH:MM` in `tz` — the top-bar clock. */
export function clockText(ms: number, tz: string = displayTimeZone()): string {
  const w = wallClock(ms, tz);
  return `${pad2(w.hour)}:${pad2(w.minute)}`;
}

/** The zone a schedule's dialog shows and saves its time in (ADR-221 decision 6): a new schedule
 *  takes the display zone, an existing one keeps its own — `null` being a row stored before zones
 *  existed, which means UTC. */
export function scheduleZone(stored: string | null | undefined, isNew: boolean): string {
  if (isNew) return displayTimeZone();
  return stored || 'UTC';
}

/** The offset from UTC of `tz` at `ms`, as NetBox's `tzoffset` writes it (`%z`): `+0900`,
 *  `-0500`, `+0530` (ADR-253 decision 4). */
export function utcOffsetText(ms: number, tz: string): string {
  const minutes = Math.round((asUtc(wallClock(ms, tz)) - Math.floor(ms / 1000) * 1000) / 60_000);
  const sign = minutes < 0 ? '-' : '+';
  const abs = Math.abs(minutes);
  return `${sign}${pad2(Math.floor(abs / 60))}${pad2(abs % 60)}`;
}

const shortDateCache = new Map<string, Intl.DateTimeFormat>();

/** The month, day and weekday of `ms` in `tz`, in `locale` — the line under a world-clock face
 *  (ADR-237). Every other date on a screen is in the display zone; this one names its own. */
export function zoneDateText(ms: number, tz: string, locale: string): string {
  const key = `${locale}|${tz}`;
  let f = shortDateCache.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, {
      timeZone: tz,
      month: 'numeric',
      day: 'numeric',
      weekday: 'short',
    });
    shortDateCache.set(key, f);
  }
  return f.format(new Date(ms));
}

const weekdayCache = new Map<string, Intl.DateTimeFormat>();

/** The short weekday of `ms` in `tz`, in `locale` (`Mon` in English) - when a site's delivery
 *  hours open again, if not today (ADR-252). */
export function zoneWeekdayText(ms: number, tz: string, locale: string): string {
  const key = `${locale}|${tz}`;
  let f = weekdayCache.get(key);
  if (!f) {
    f = new Intl.DateTimeFormat(locale, { timeZone: tz, weekday: 'short' });
    weekdayCache.set(key, f);
  }
  return f.format(new Date(ms));
}
