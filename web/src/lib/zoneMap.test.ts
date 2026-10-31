// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { project } from '../pages/geoProjection';
import { isKnownTimeZone } from './timeZone';
import {
  COMMON_TIME_ZONES,
  ZONE_MAP_BOTTOM,
  ZONE_MAP_POINTS,
  ZONE_MAP_TOP,
  countryName,
  initialZoneRows,
  matchTimeZones,
  nearestPoint,
  pointLabel,
  sameOffset,
  utcOffsetMinutes,
  zoneCountry,
} from './zoneMap';

const at = (iso: string) => Date.parse(iso);

describe('the map points', () => {
  it('are fifty zones this runtime knows, each once, each a real country', () => {
    expect(ZONE_MAP_POINTS).toHaveLength(50);
    const zones = ZONE_MAP_POINTS.map((p) => p.zone);
    expect(new Set(zones).size).toBe(zones.length);
    for (const p of ZONE_MAP_POINTS) {
      expect(isKnownTimeZone(p.zone), p.zone).toBe(true);
      expect(p.country, p.zone).toMatch(/^[A-Z]{2}$/);
      expect(countryName(p.country, 'en'), p.zone).not.toBe(p.country);
    }
  });

  it('all sit inside the latitudes the map shows', () => {
    for (const p of ZONE_MAP_POINTS) {
      const { y } = project(p.lat, p.lon);
      expect(y, p.zone).toBeGreaterThan(ZONE_MAP_TOP);
      expect(y, p.zone).toBeLessThan(ZONE_MAP_BOTTOM);
    }
  });

  it('shows India at New Delhi and saves Asia/Kolkata', () => {
    const india = ZONE_MAP_POINTS.find((p) => p.country === 'IN');
    expect(india?.zone).toBe('Asia/Kolkata');
    expect(pointLabel(india!)).toBe('New Delhi');
    expect(india!.lat).toBeCloseTo(28.6, 0);
  });

  it('names every other point after the city in its zone', () => {
    const ny = ZONE_MAP_POINTS.find((p) => p.zone === 'America/New_York')!;
    expect(pointLabel(ny)).toBe('New York');
    expect(pointLabel(ZONE_MAP_POINTS.find((p) => p.zone.endsWith('Buenos_Aires'))!)).toBe(
      'Buenos Aires',
    );
  });

  it('are the common zones, after UTC', () => {
    expect(COMMON_TIME_ZONES[0]).toBe('UTC');
    expect(COMMON_TIME_ZONES.slice(1)).toEqual(ZONE_MAP_POINTS.map((p) => p.zone));
  });
});

describe('a click on the map', () => {
  it('chooses the point under it, or the nearest within the radius', () => {
    const tokyo = project(35.65, 139.74);
    expect(nearestPoint(tokyo.x, tokyo.y, 5)?.zone).toBe('Asia/Tokyo');
    expect(nearestPoint(tokyo.x + 2, tokyo.y + 6, 20)?.zone).toBe('Asia/Tokyo');
  });

  it('chooses nothing in the open sea', () => {
    const midPacific = project(-30, -130);
    expect(nearestPoint(midPacific.x, midPacific.y, 20)).toBeNull();
  });
});

describe('the clock a point reads', () => {
  it('counts the offset with daylight saving', () => {
    expect(utcOffsetMinutes('Asia/Tokyo', at('2026-07-01T00:00:00Z'))).toBe(540);
    expect(utcOffsetMinutes('Asia/Kolkata', at('2026-07-01T00:00:00Z'))).toBe(330);
    expect(utcOffsetMinutes('America/New_York', at('2026-07-01T00:00:00Z'))).toBe(-240);
    expect(utcOffsetMinutes('America/New_York', at('2026-12-01T00:00:00Z'))).toBe(-300);
  });

  it('matches zones that read the same clock now, and not by name', () => {
    expect(sameOffset('Asia/Tokyo', 'Asia/Seoul', at('2026-07-01T00:00:00Z'))).toBe(true);
    // London and Lagos agree only while London is on summer time.
    expect(sameOffset('Europe/London', 'Africa/Lagos', at('2026-07-01T00:00:00Z'))).toBe(true);
    expect(sameOffset('Europe/London', 'Africa/Lagos', at('2026-12-01T00:00:00Z'))).toBe(false);
    expect(sameOffset('Europe/London', 'Atlantic/Reykjavik', at('2026-12-01T00:00:00Z'))).toBe(
      true,
    );
  });
});

describe('the picker', () => {
  it('lists the common zones before anything is typed, and matches a space as an underscore', () => {
    expect(matchTimeZones('')).toContain('Asia/Tokyo');
    expect(matchTimeZones('new york', 'en', ['America/New_York', 'Asia/Tokyo'])).toEqual([
      'America/New_York',
    ]);
  });

  it('finds India by its country name in English and in Japanese, and by New Delhi', () => {
    for (const [q, lang] of [
      ['india', 'en'],
      ['インド', 'ja'],
      ['delhi', 'en'],
    ]) {
      expect(matchTimeZones(q, lang, [])[0], q).toBe('Asia/Kolkata');
    }
    // A name that is the start of another country's comes before it, and still finds both.
    expect(matchTimeZones('インド', 'ja', [])).toEqual(['Asia/Kolkata', 'Asia/Jakarta']);
  });

  it('puts a point found by its country before zones found by their name', () => {
    expect(matchTimeZones('japan', 'en', ['Asia/Tokyo', 'Japan'])).toEqual(['Asia/Tokyo', 'Japan']);
  });

  it('names the country only for a zone on the map', () => {
    expect(zoneCountry('Asia/Kolkata', 'en')).toBe('India');
    expect(zoneCountry('Asia/Kolkata', 'ja')).toBe('インド');
    expect(zoneCountry('Etc/GMT+3', 'en')).toBe('');
  });

  it('leaves a desktop to its map until something is typed', () => {
    const common = matchTimeZones('');
    expect(initialZoneRows(common, '', false, null)).toEqual([]);
    expect(initialZoneRows(common, '', false, 'Asia/Tokyo')).toEqual(['Asia/Tokyo']);
    expect(initialZoneRows(common, '', true, null)).toEqual(common);
    expect(initialZoneRows(['Asia/Tokyo'], 'tok', false, null)).toEqual(['Asia/Tokyo']);
  });
});
