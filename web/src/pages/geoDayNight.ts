// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Geo map's day/night shading (ADR-189): where the Sun is now, and the shape of the
// part of the world it is below the horizon for — in the same 720 × 360 grid `geoProjection.ts`
// projects into, so the shading is one more path under the pins.
//
// Computed in the browser from the browser's clock. No server, no data file: the Sun's position is
// a closed-form approximation (declination, right ascension, sidereal time) good to about 0.01°,
// which is far below a pixel at any zoom this page allows. ⚠️ A browser whose clock is wrong draws
// the terminator wrong by the same amount — ten minutes is 2.5° of longitude. That is why the
// legend states which UTC time the sky is drawn for.
//
// Everything here is pure so it runs under Vitest; `GeoMapPage.tsx` only renders the strings.

import { MAP_HEIGHT, MAP_WIDTH } from './geoProjection';

const RAD = Math.PI / 180;
const DEG = 180 / Math.PI;

/** How far below the horizon the Sun is at each edge of the four shaded steps, in degrees:
 *  sunset (0.833° — refraction plus the Sun's radius, so "down" means the whole disc is gone),
 *  then the end of civil, nautical and astronomical twilight. The astronomical definitions, not a
 *  design choice: an operator reading "twilight" should get the thing the word means. */
export const TWILIGHT_LEVELS = [0.833, 6, 12, 18] as const;

/** Where the Sun is overhead. */
export interface SubsolarPoint {
  lat: number;
  lon: number;
}

/** The subsolar point at `date`, in degrees. Longitude is normalised to [-180, 180). */
export function sunPosition(date: Date): SubsolarPoint {
  // Days since J2000.0 (2000-01-01 12:00 TT; the TT/UTC difference is ~70 s, below what this draws).
  const n = date.getTime() / 86_400_000 + 2440587.5 - 2451545.0;
  const meanLon = 280.46 + 0.9856474 * n;
  const anomaly = (357.528 + 0.9856003 * n) * RAD;
  const eclipticLon = (meanLon + 1.915 * Math.sin(anomaly) + 0.02 * Math.sin(2 * anomaly)) * RAD;
  const obliquity = (23.439 - 0.0000004 * n) * RAD;
  const dec = Math.asin(Math.sin(obliquity) * Math.sin(eclipticLon));
  const ra = Math.atan2(Math.cos(obliquity) * Math.sin(eclipticLon), Math.cos(eclipticLon)) * DEG;
  const gmst = 280.46061837 + 360.98564736629 * n;
  return { lat: dec * DEG, lon: normaliseLon(ra - gmst) };
}

function normaliseLon(lon: number): number {
  return ((((lon + 180) % 360) + 360) % 360) - 180;
}

/** The Sun's altitude above the horizon at a place, in degrees (negative = below). */
export function sunAltitude(sun: SubsolarPoint, lat: number, lon: number): number {
  const d = sun.lat * RAD;
  const p = lat * RAD;
  const h = (lon - sun.lon) * RAD;
  const s = Math.sin(p) * Math.sin(d) + Math.cos(p) * Math.cos(d) * Math.cos(h);
  return Math.asin(Math.max(-1, Math.min(1, s))) * DEG;
}

/** What a site's sky is doing, for its tooltip: the same three words as the legend. */
export type Sky = 'day' | 'twilight' | 'night';

export function skyAt(altitude: number): Sky {
  if (altitude > -TWILIGHT_LEVELS[0]) return 'day';
  if (altitude > -TWILIGHT_LEVELS[TWILIGHT_LEVELS.length - 1]) return 'twilight';
  return 'night';
}

/** One meridian's dark stretch: the latitudes at which the Sun is below the level. */
interface DarkColumn {
  lo: number;
  hi: number;
}

const LAT_STEP = 0.5;
const LAT_N = Math.round(180 / LAT_STEP) + 1;
/** Longitude sampling. At the 240× ceiling (ADR-188) a 0.25° step is 120 px, along a curve gentle
 *  enough that the polyline does not read as one. */
const LON_STEP = 0.25;
const LON_N = Math.round(360 / LON_STEP) + 1;

const SIN_LAT = new Float64Array(LAT_N);
const COS_LAT = new Float64Array(LAT_N);
for (let j = 0; j < LAT_N; j++) {
  const p = (-90 + j * LAT_STEP) * RAD;
  SIN_LAT[j] = Math.sin(p);
  COS_LAT[j] = Math.cos(p);
}

/**
 * For each sampled meridian, the stretch of latitudes where the Sun is more than `level` degrees
 * below the horizon, or `null` where there is none.
 *
 * ⚠️ Solved per meridian rather than as "the terminator latitude for each longitude" because that
 * formula (`tan φ = −cos H / tan δ`) assumes the dark region touches a pole. It does not always:
 * near an equinox the region below 18° is a cap around the anti-solar point that reaches neither
 * pole, and the one-sided formula would shade the wrong half. What *is* always true is that a
 * meridian (half a great circle) crosses that cap in at most one stretch, so the answer per column
 * is one interval — found on a grid and refined by bisection at each end.
 */
export function darkColumns(sun: SubsolarPoint, level: number): (DarkColumn | null)[] {
  const c = -Math.sin(level * RAD);
  const sd = Math.sin(sun.lat * RAD);
  const cd = Math.cos(sun.lat * RAD);
  const cols: (DarkColumn | null)[] = new Array(LON_N);
  for (let i = 0; i < LON_N; i++) {
    const lon = -180 + i * LON_STEP;
    const b = cd * Math.cos((lon - sun.lon) * RAD);
    // sin(altitude) − sin(−level): negative means darker than the level.
    const f = (lat: number) => sd * Math.sin(lat * RAD) + b * Math.cos(lat * RAD) - c;
    let first = -1;
    let last = -1;
    for (let j = 0; j < LAT_N; j++) {
      if (sd * SIN_LAT[j] + b * COS_LAT[j] < c) {
        if (first < 0) first = j;
        last = j;
      }
    }
    if (first < 0) {
      cols[i] = null;
      continue;
    }
    const edge = (inside: number, outside: number) => {
      let a = inside;
      let z = outside;
      for (let k = 0; k < 22; k++) {
        const m = (a + z) / 2;
        if (f(m) < 0) a = m;
        else z = m;
      }
      return (a + z) / 2;
    };
    const lat = (j: number) => -90 + j * LAT_STEP;
    cols[i] = {
      lo: first === 0 ? -90 : edge(lat(first), lat(first - 1)),
      hi: last === LAT_N - 1 ? 90 : edge(lat(last), lat(last + 1)),
    };
  }
  return cols;
}

const round3 = (v: number) => Math.round(v * 1000) / 1000;
const x = (lon: number) => round3(((lon + 180) / 360) * MAP_WIDTH);
const y = (lat: number) => round3(((90 - lat) / 180) * MAP_HEIGHT);

/**
 * SVG path data for the region darker than `level`, as ONE closed polygon: the upper edge west to
 * east, then the lower edge back.
 *
 * A meridian with no dark stretch is collapsed onto the middle of the nearest column that has one,
 * so it contributes a zero-area line rather than a sliver. Nearest by index, not "the last one
 * seen": carrying the previous run's middle across a gap would join two runs with a thin wedge.
 * Returns `''` when nothing is darker than the level.
 */
export function darkPath(sun: SubsolarPoint, level: number): string {
  const cols = darkColumns(sun, level);
  const mids = new Array<number>(LON_N);
  const dist = new Array<number>(LON_N).fill(Infinity);
  let any = false;
  for (const reverse of [false, true]) {
    let seen = -1;
    for (let k = 0; k < LON_N; k++) {
      const i = reverse ? LON_N - 1 - k : k;
      const col = cols[i];
      if (col) {
        seen = i;
        any = true;
      }
      if (seen >= 0 && Math.abs(i - seen) < dist[i]) {
        const s = cols[seen] as DarkColumn;
        dist[i] = Math.abs(i - seen);
        mids[i] = (s.lo + s.hi) / 2;
      }
    }
  }
  if (!any) return '';
  const up: string[] = [];
  const down: string[] = [];
  for (let i = 0; i < LON_N; i++) {
    const px = x(-180 + i * LON_STEP);
    const col = cols[i];
    up.push(`${px} ${y(col ? col.hi : mids[i])}`);
    down.push(`${px} ${y(col ? col.lo : mids[i])}`);
  }
  return `M${up.join('L')}L${down.reverse().join('L')}Z`;
}

/** The four stacked shading paths, lightest (sunset) first. */
export function twilightPaths(date: Date): string[] {
  const sun = sunPosition(date);
  return TWILIGHT_LEVELS.map((level) => darkPath(sun, level));
}
