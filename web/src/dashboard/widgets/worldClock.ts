// SPDX-License-Identifier: AGPL-3.0-only
// What the "World clock" widget shows, given its persisted settings and the current instant
// (ADR-237).
//
// Everything comes from the browser's clock, read through `lib/timeZone.ts` (ADR-221 decision 8:
// nothing else turns an instant into wall-clock text). The widget reads no API, which is why its
// `reads` is empty and why it may sit on the public board without opening anything.
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`, so
// judgement left in the `.tsx` is judgement nothing tests.

import {
  canonicalTimeZone,
  displayTimeZone,
  isKnownTimeZone,
  knownTimeZones,
  wallClock,
} from '../../lib/timeZone';
import type { WidgetSettings } from '../types';

/** How many zones one card may hold. Eight tiles fill two rows of a 12-column card; past that the
 *  card stops being glanceable. */
export const MAX_ZONES = 8;

/** The zone that follows whoever is looking — the viewer's display zone (ADR-221: the browser's,
 *  unless they picked one with the top-bar clock). Stored as this token rather than as a resolved
 *  name, so a shared or public board shows each viewer their own time, not the composer's. */
export const LOCAL_ZONE = 'local';

/** One clock on the card. `label` is the operator's name for it; empty means "use the city". */
export interface ClockZone {
  tz: string;
  label: string;
}

/** The widget's persisted settings. */
export interface ClockSettings {
  zones: ClockZone[];
  hour12: boolean;
  seconds: boolean;
}

/** What a card placed with no settings shows: the viewer's time and UTC — never an empty card. */
export const DEFAULT_ZONES: readonly ClockZone[] = [
  { tz: LOCAL_ZONE, label: '' },
  { tz: 'UTC', label: '' },
];

/**
 * Read the settings out of the opaque bag, dropping anything malformed.
 *
 * The bag is user-editable JSON that has round-tripped through localStorage and the server. A zone
 * whose name this browser does not know is dropped rather than rendered as an exception, duplicates
 * collapse, and the list is cut at {@link MAX_ZONES}. An absent `zones` is the default pair; an
 * empty array is the operator's own choice and stays empty.
 */
export function readClockSettings(s: WidgetSettings = {}): ClockSettings {
  let zones: ClockZone[];
  if (Array.isArray(s.zones)) {
    zones = [];
    for (const raw of s.zones) {
      if (zones.length >= MAX_ZONES) break;
      if (typeof raw !== 'object' || raw === null) continue;
      const { tz, label } = raw as Record<string, unknown>;
      if (typeof tz !== 'string' || (tz !== LOCAL_ZONE && !isKnownTimeZone(tz))) continue;
      if (zones.some((z) => z.tz === tz)) continue;
      zones.push({ tz, label: typeof label === 'string' ? label : '' });
    }
  } else {
    zones = DEFAULT_ZONES.map((z) => ({ ...z }));
  }
  return { zones, hour12: s.hour12 === true, seconds: s.seconds !== false };
}

/**
 * Turn what the operator typed into a zone to store, or `null` when nothing matches.
 *
 * The listed names are matched ignoring case first, so `asia/tokyo` stores `Asia/Tokyo`. A name the
 * list lacks but `Intl` accepts (an alias) stores whatever `Intl` resolves it to — ⚠️ which can be
 * the older spelling: Chromium resolves `Asia/Kolkata` to `Asia/Calcutta` (ADR-237, risks).
 * `localName` is the word the picker offers for {@link LOCAL_ZONE}.
 */
export function resolveTimeZone(
  input: string,
  localName: string,
  known: readonly string[] = knownTimeZones(),
): string | null {
  const s = input.trim();
  if (s === '') return null;
  const lower = s.toLowerCase();
  if (lower === LOCAL_ZONE || lower === localName.toLowerCase()) return LOCAL_ZONE;
  const hit = known.find((z) => z.toLowerCase() === lower);
  if (hit) return hit;
  return canonicalTimeZone(s);
}

export type AddZoneResult =
  | { ok: true; zones: ClockZone[] }
  | { ok: false; reason: 'unknown' | 'duplicate' | 'full' };

/** Append a typed zone, or say why it was refused. */
export function addZone(
  zones: readonly ClockZone[],
  input: string,
  localName: string,
  known?: readonly string[],
): AddZoneResult {
  if (zones.length >= MAX_ZONES) return { ok: false, reason: 'full' };
  const tz = resolveTimeZone(input, localName, known);
  if (tz === null) return { ok: false, reason: 'unknown' };
  if (zones.some((z) => z.tz === tz)) return { ok: false, reason: 'duplicate' };
  return { ok: true, zones: [...zones, { tz, label: '' }] };
}

/** Swap zone `i` with its neighbour `delta` places away; out of range is a no-op. */
export function moveZone(zones: readonly ClockZone[], i: number, delta: -1 | 1): ClockZone[] {
  const j = i + delta;
  const out = [...zones];
  if (i < 0 || i >= out.length || j < 0 || j >= out.length) return out;
  [out[i], out[j]] = [out[j], out[i]];
  return out;
}

/** The city part of an IANA name, for a zone with no label: `America/New_York` → `New York`.
 *  `null` for {@link LOCAL_ZONE}, whose name is a translated word the caller supplies. */
export function cityName(tz: string): string | null {
  if (tz === LOCAL_ZONE) return null;
  return (tz.split('/').pop() ?? tz).replace(/_/g, ' ');
}

/** One zone at one instant. */
export interface ZoneReading {
  /** Two-digit hour, minute and second, in the chosen hour cycle. */
  hh: string;
  mm: string;
  ss: string;
  /** `AM` / `PM` in the 12-hour cycle, otherwise empty. */
  dayPeriod: string;
  /** Before 06:00 or from 18:00, local to the zone. */
  night: boolean;
  /** The zone's calendar date against the viewer's: -1 the day before, 1 the day after. */
  dayShift: -1 | 0 | 1;
  /** `UTC`, `UTC+9`, `UTC-3:30`. */
  offset: string;
}

/** Minutes east of UTC, from the difference between the zone's wall clock and the instant —
 *  computed rather than read from `timeZoneName: 'shortOffset'`, which older engines lack. */
function offsetMinutes(tz: string, now: Date): number {
  const w = wallClock(now.getTime(), tz);
  const asUtc = Date.UTC(w.year, w.month - 1, w.day, w.hour, w.minute, w.second);
  const instant = Math.floor(now.getTime() / 1000) * 1000;
  return Math.round((asUtc - instant) / 60000);
}

function formatOffset(min: number): string {
  if (min === 0) return 'UTC';
  const sign = min > 0 ? '+' : '-';
  const abs = Math.abs(min);
  const h = Math.floor(abs / 60);
  const m = abs % 60;
  return `UTC${sign}${h}${m === 0 ? '' : `:${String(m).padStart(2, '0')}`}`;
}

const pad = (n: number) => String(n).padStart(2, '0');

/** Read zone `tz` (an IANA name — resolve {@link LOCAL_ZONE} first) at `now`, as seen by a viewer
 *  in `viewerTz`. */
export function readZone(tz: string, now: Date, hour12: boolean, viewerTz: string): ZoneReading {
  const w = wallClock(now.getTime(), tz);
  const v = wallClock(now.getTime(), viewerTz);
  const zoneDay = Date.UTC(w.year, w.month - 1, w.day);
  const viewerDay = Date.UTC(v.year, v.month - 1, v.day);
  const dayShift = zoneDay < viewerDay ? -1 : zoneDay > viewerDay ? 1 : 0;
  const h12 = w.hour % 12 === 0 ? 12 : w.hour % 12;
  return {
    hh: pad(hour12 ? h12 : w.hour),
    mm: pad(w.minute),
    ss: pad(w.second),
    dayPeriod: hour12 ? (w.hour < 12 ? 'AM' : 'PM') : '',
    night: w.hour < 6 || w.hour >= 18,
    dayShift,
    offset: formatOffset(offsetMinutes(tz, now)),
  };
}

/** The viewer's zone: the display zone every other time on the screen is shown in. */
export const viewerTimeZone = displayTimeZone;

/** The IANA name to read for a stored zone. */
export const realTimeZone = (tz: string, viewerTz: string) => (tz === LOCAL_ZONE ? viewerTz : tz);
