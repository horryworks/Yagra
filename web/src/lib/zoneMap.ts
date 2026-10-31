// SPDX-License-Identifier: AGPL-3.0-only
// The top-bar clock's world map and search (ADR-221 Inc.2): which zones sit on the map, what each
// point is called, which point a click lands on, and which zones a typed word finds.
//
// The points are `zoneMapPoints.json`, one row per zone: the IANA name that is saved, where the
// point is drawn (tzdata's `zone1970.tab`, except India, drawn at New Delhi by decision), the
// country, and a display name only where the zone's own city is not the one to show. A Rust test
// in `api/util.rs` reads the same file and fails if core cannot parse a zone on it, because a zone
// the map offers and core refuses would be saved by the clock and refused by every schedule form.
//
// Pure, so it runs under Vitest; `ZoneMap.tsx` and `ZoneClock.tsx` only draw what this returns.

import { project } from '../pages/geoProjection';
import { knownTimeZones, wallClock } from './timeZone';
import POINTS from './zoneMapPoints.json';

export interface ZoneMapPoint {
  zone: string;
  lat: number;
  lon: number;
  /** ISO 3166-1 alpha-2. */
  country: string;
  /** Shown instead of the city in the zone's name. */
  name?: string;
}

export const ZONE_MAP_POINTS: readonly ZoneMapPoint[] = POINTS;

/** The latitudes the map shows: Antarctica and the far north carry no point and would only take
 *  height from the popover. In `geoProjection` units, so the outline and the night side need no
 *  conversion. */
export const ZONE_MAP_TOP = project(78, 0).y;
export const ZONE_MAP_BOTTOM = project(-58, 0).y;

/** The zones the picker lists before anything is typed, where there is no map to browse. */
export const COMMON_TIME_ZONES: readonly string[] = ['UTC', ...ZONE_MAP_POINTS.map((p) => p.zone)];

const byZone = new Map(ZONE_MAP_POINTS.map((p) => [p.zone, p]));

/** The map point for `zone`, if it has one. */
export function zonePoint(zone: string): ZoneMapPoint | undefined {
  return byZone.get(zone);
}

/** What a point is called: its display name, else the last part of its zone (`New_York` reads
 *  `New York`). */
export function pointLabel(p: ZoneMapPoint): string {
  return p.name ?? (p.zone.split('/').pop() ?? p.zone).replace(/_/g, ' ');
}

const regionNames = new Map<string, Intl.DisplayNames | null>();

/** A country's name in `lang`, or `''` when this runtime cannot name it. */
export function countryName(code: string, lang: string): string {
  let names = regionNames.get(lang);
  if (names === undefined) {
    try {
      names = new Intl.DisplayNames([lang], { type: 'region' });
    } catch {
      names = null;
    }
    regionNames.set(lang, names);
  }
  try {
    return names?.of(code) ?? '';
  } catch {
    return '';
  }
}

/** The country shown beside a zone in the list, or `''` for a zone with no point. */
export function zoneCountry(zone: string, lang: string): string {
  const p = byZone.get(zone);
  return p ? countryName(p.country, lang) : '';
}

/** Minutes ahead of UTC that `tz` is at `ms`. */
export function utcOffsetMinutes(tz: string, ms: number): number {
  const w = wallClock(ms, tz);
  const wall = Date.UTC(w.year, w.month - 1, w.day, w.hour, w.minute, w.second);
  return Math.round((wall - Math.floor(ms / 1000) * 1000) / 60_000);
}

/** Whether two zones read the same clock at `ms`. */
export function sameOffset(a: string, b: string, ms: number): boolean {
  return utcOffsetMinutes(a, ms) === utcOffsetMinutes(b, ms);
}

/** The point nearest `(x, y)` within `radius`, all in map units; `null` when none is that close. */
export function nearestPoint(
  x: number,
  y: number,
  radius: number,
  points: readonly ZoneMapPoint[] = ZONE_MAP_POINTS,
): ZoneMapPoint | null {
  let best: ZoneMapPoint | null = null;
  let bestD = radius * radius;
  for (const p of points) {
    const at = project(p.lat, p.lon);
    const d = (at.x - x) ** 2 + (at.y - y) ** 2;
    if (d <= bestD) {
      best = p;
      bestD = d;
    }
  }
  return best;
}

/** How well a point answers to `query`: `0` when one of its names is exactly that, `1` when one
 *  starts with it, `2` when one contains it, `null` when none does. Its names are its zone, its
 *  display name, and its country in English and in `lang`. Case-insensitive; a space matches the
 *  `_` in a zone name. The rank is what puts India before Indonesia when a Japanese query names
 *  India. */
export function pointRank(p: ZoneMapPoint, query: string, lang: string): number | null {
  const q = query.trim().toLowerCase();
  if (!q) return 0;
  const names = [
    [p.zone.toLowerCase(), q.replace(/\s+/g, '_')],
    [pointLabel(p).toLowerCase(), q],
    [countryName(p.country, 'en').toLowerCase(), q],
    [countryName(p.country, lang).toLowerCase(), q],
  ];
  let best: number | null = null;
  for (const [name, want] of names) {
    const r = name === want ? 0 : name.startsWith(want) ? 1 : name.includes(want) ? 2 : null;
    if (r != null && (best == null || r < best)) best = r;
  }
  return best;
}

/** Whether a point answers to `query` at all. */
export function pointMatches(p: ZoneMapPoint, query: string, lang: string): boolean {
  return pointRank(p, query, lang) != null;
}

/** The picker's matches for what was typed: map points answering to it first (so `india` finds
 *  `Asia/Kolkata`), then every zone whose name contains it. Empty input lists the common zones.
 *  Capped so a one-letter query stays short. */
export function matchTimeZones(
  query: string,
  lang = 'en',
  all: string[] = knownTimeZones(),
  cap = 40,
): string[] {
  const q = query.trim().toLowerCase().replace(/\s+/g, '_');
  if (!q) return [...COMMON_TIME_ZONES];
  const ranked = ZONE_MAP_POINTS.map((p) => ({ zone: p.zone, rank: pointRank(p, query, lang) }))
    .filter((r): r is { zone: string; rank: number } => r.rank != null)
    .sort((x, y) => x.rank - y.rank);
  const found = new Set(ranked.map((r) => r.zone));
  for (const z of all) if (z.toLowerCase().includes(q)) found.add(z);
  return [...found].slice(0, cap);
}

/** The rows under "follow the browser". While something is typed, the matches. Before that, a
 *  phone lists the common zones, while a desktop has the map to browse and lists only the zone
 *  chosen, if one is. */
export function initialZoneRows(
  matches: string[],
  query: string,
  phone: boolean,
  chosen: string | null,
): string[] {
  if (query.trim() || phone) return matches;
  return chosen ? [chosen] : [];
}
