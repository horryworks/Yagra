// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  darkColumns,
  darkPath,
  skyAt,
  sunAltitude,
  sunPosition,
  TWILIGHT_LEVELS,
  twilightPaths,
} from './geoDayNight';

describe('sunPosition', () => {
  it('puts the Sun where an almanac does', () => {
    // 2026-09-30 12:00 UTC: declination about −2.9° (a week after the equinox), and the Sun is
    // ~10 minutes ahead of the clock (equation of time), so it is overhead at about 2.5°W.
    const s = sunPosition(new Date(Date.UTC(2026, 8, 30, 12, 0)));
    expect(s.lat).toBeCloseTo(-2.9, 0);
    expect(Math.abs(s.lon - -2.5)).toBeLessThan(0.5);
  });

  it('reaches the tropics at the solstices', () => {
    expect(sunPosition(new Date(Date.UTC(2026, 5, 21, 12))).lat).toBeCloseTo(23.44, 1);
    expect(sunPosition(new Date(Date.UTC(2026, 11, 21, 12))).lat).toBeCloseTo(-23.44, 1);
  });

  it('moves 15° west an hour and keeps longitude in [-180, 180)', () => {
    const a = sunPosition(new Date(Date.UTC(2026, 8, 30, 12)));
    const b = sunPosition(new Date(Date.UTC(2026, 8, 30, 13)));
    expect(a.lon - b.lon).toBeCloseTo(15, 0);
    for (let h = 0; h < 24; h++) {
      const { lon } = sunPosition(new Date(Date.UTC(2026, 8, 30, h)));
      expect(lon).toBeGreaterThanOrEqual(-180);
      expect(lon).toBeLessThan(180);
    }
  });
});

describe('sunAltitude / skyAt', () => {
  const noonUtc = sunPosition(new Date(Date.UTC(2026, 8, 30, 12)));

  it('is day in London and night in Tokyo at noon UTC', () => {
    expect(skyAt(sunAltitude(noonUtc, 51.5, -0.13))).toBe('day');
    expect(skyAt(sunAltitude(noonUtc, 35.68, 139.76))).toBe('night');
  });

  it('draws the twilight between the sunset line and 18° below', () => {
    expect(skyAt(0)).toBe('day'); // half the disc still up
    expect(skyAt(-1)).toBe('twilight');
    expect(skyAt(-17.9)).toBe('twilight');
    expect(skyAt(-18.1)).toBe('night');
  });

  it('lights the summer pole and darkens the winter one', () => {
    const june = sunPosition(new Date(Date.UTC(2026, 5, 21, 12)));
    expect(sunAltitude(june, 89, 0)).toBeGreaterThan(0);
    expect(sunAltitude(june, -89, 0)).toBeLessThan(0);
  });
});

describe('darkColumns', () => {
  it('puts each edge exactly at the level', () => {
    const sun = sunPosition(new Date(Date.UTC(2026, 8, 30, 12)));
    for (const level of TWILIGHT_LEVELS) {
      const cols = darkColumns(sun, level);
      let checked = 0;
      cols.forEach((col, i) => {
        if (!col || i % 40 !== 0) return;
        const lon = -180 + i * 0.25;
        for (const lat of [col.lo, col.hi]) {
          if (Math.abs(lat) === 90) continue; // the dark stretch runs into the pole
          expect(sunAltitude(sun, lat, lon)).toBeCloseTo(-level, 2);
          checked += 1;
        }
      });
      expect(checked).toBeGreaterThan(5);
    }
  });

  it('shades a cap that reaches neither pole near an equinox', () => {
    // The case the one-sided terminator formula gets wrong: with the Sun on the equator, the region
    // more than 18° down is a cap of radius 72° round the anti-solar point.
    const sun = { lat: 0, lon: 0 };
    const cols = darkColumns(sun, 18).filter((c) => c !== null);
    expect(cols.length).toBeGreaterThan(0);
    for (const c of cols) {
      expect(c.hi).toBeLessThan(90);
      expect(c.lo).toBeGreaterThan(-90);
    }
    // Opposite the Sun (column for 180°E) the cap spans ±72°.
    const antipode = darkColumns(sun, 18)[1440];
    expect(antipode?.hi).toBeCloseTo(72, 1);
    expect(antipode?.lo).toBeCloseTo(-72, 1);
  });

  it('runs into the winter pole at a solstice', () => {
    const june = sunPosition(new Date(Date.UTC(2026, 5, 21, 12)));
    const cols = darkColumns(june, TWILIGHT_LEVELS[0]);
    expect(cols.every((c) => c === null || c.lo === -90)).toBe(true);
  });
});

describe('darkPath / twilightPaths', () => {
  it('draws four closed paths inside the map, with no NaN', () => {
    for (const date of [
      new Date(Date.UTC(2026, 8, 30, 12)),
      new Date(Date.UTC(2026, 2, 20, 14, 46)), // equinox
      new Date(Date.UTC(2026, 5, 21, 0)), // solstice
    ]) {
      const paths = twilightPaths(date);
      expect(paths).toHaveLength(4);
      for (const d of paths) {
        expect(d).toMatch(/^M.*Z$/);
        expect(d).not.toContain('NaN');
        const nums = d.match(/-?\d+(\.\d+)?/g)!.map(Number);
        for (let k = 0; k < nums.length; k += 2) {
          expect(nums[k]).toBeGreaterThanOrEqual(0);
          expect(nums[k]).toBeLessThanOrEqual(720);
          expect(nums[k + 1]).toBeGreaterThanOrEqual(0);
          expect(nums[k + 1]).toBeLessThanOrEqual(360);
        }
      }
    }
  });

  it('shades more of the world for a shallower level', () => {
    // The four steps stack: each level's region contains the next one's, so the sunset path must
    // enclose the most area. Compared by how many columns carry a dark stretch.
    const sun = { lat: 0, lon: 0 };
    const width = (level: number) => darkColumns(sun, level).filter((c) => c !== null).length;
    const widths = TWILIGHT_LEVELS.map(width);
    for (let k = 1; k < widths.length; k++) expect(widths[k]).toBeLessThanOrEqual(widths[k - 1]);
    expect(darkPath(sun, TWILIGHT_LEVELS[0])).not.toBe('');
  });
});
