// SPDX-License-Identifier: AGPL-3.0-only
import { afterEach, describe, expect, it } from 'vitest';
import {
  browserTimeZone,
  clockText,
  displayTimeZone,
  isKnownTimeZone,
  isZoneChosen,
  setDisplayTimeZone,
  wallClock,
  zonedWallToMs,
  zoneAbbreviation,
} from './timeZone';
import { readSources } from '../testSupport/sources';

const at = (iso: string) => Date.parse(iso);

afterEach(() => setDisplayTimeZone(null));

describe('the display zone', () => {
  it('follows the browser until a zone is chosen, and again after "follow browser"', () => {
    expect(displayTimeZone()).toBe(browserTimeZone());
    expect(isZoneChosen()).toBe(false);
    setDisplayTimeZone('America/New_York');
    expect(displayTimeZone()).toBe('America/New_York');
    expect(isZoneChosen()).toBe(true);
    setDisplayTimeZone(null);
    expect(displayTimeZone()).toBe(browserTimeZone());
  });

  it('ignores a name this runtime does not know rather than showing UTC under its name', () => {
    setDisplayTimeZone('Mars/Olympus_Mons');
    expect(isZoneChosen()).toBe(false);
    expect(isKnownTimeZone('Mars/Olympus_Mons')).toBe(false);
    expect(isKnownTimeZone('Asia/Tokyo')).toBe(true);
  });
});

describe('a wall clock in a zone', () => {
  it('reads what a clock there says', () => {
    expect(wallClock(at('2026-10-30T23:30:00Z'), 'Asia/Tokyo')).toMatchObject({
      year: 2026,
      month: 10,
      day: 31,
      hour: 8,
      minute: 30,
      weekday: 6,
    });
    expect(clockText(at('2026-10-30T23:30:00Z'), 'Asia/Tokyo')).toBe('08:30');
  });

  it('turns an ordinary wall time into its one instant', () => {
    expect(zonedWallToMs(2026, 11, 1, 7, 0, 'Asia/Tokyo')).toBe(at('2026-10-31T22:00:00Z'));
  });

  // The two daylight-saving rules are core's (`yagra_base::wall_clock::resolve`); these are the
  // same cases its tests name, so the screen predicts what core will do.
  it('resolves a wall time inside the spring gap to the end of the gap', () => {
    expect(zonedWallToMs(2027, 3, 14, 2, 30, 'America/New_York')).toBe(at('2027-03-14T07:00:00Z'));
  });

  it('resolves a wall time inside the autumn overlap to its first occurrence', () => {
    expect(zonedWallToMs(2026, 11, 1, 1, 30, 'America/New_York')).toBe(at('2026-11-01T05:30:00Z'));
  });

  it('names the zone the way a device would print it', () => {
    expect(zoneAbbreviation('UTC', at('2026-10-30T00:00:00Z'))).toBe('UTC');
    expect(zoneAbbreviation('America/New_York', at('2026-07-01T00:00:00Z'))).toBe('EDT');
    expect(zoneAbbreviation('America/New_York', at('2026-12-01T00:00:00Z'))).toBe('EST');
  });
});

// ADR-221 decision 8: a time formatted outside these two files ignores the display zone.
describe('no screen formats a date or a time by hand', () => {
  const ALLOWED = new Set(['lib/format.ts', 'lib/timeZone.ts']);
  // Built so this file's own text cannot match: a needle written out would match the line it is on.
  const NEEDLES = [
    'toLocale' + 'DateString(',
    'toLocale' + 'TimeString(',
    ...['Hours', 'Minutes', 'Date', 'Month', 'FullYear', 'Day', 'Seconds'].map(
      (f) => '.get' + f + '(',
    ),
    'Intl.Date' + 'TimeFormat(',
  ];
  // A date's `toLocaleString` with no zone; a number's (`packets.toLocaleString()`) is not a time.
  const DATE_TO_LOCALE = new RegExp('new Date\\([^)]*\\)\\.toLocale' + 'String\\(');

  // A comment that names `Date.getDay()` order is not a call.
  const isComment = (line: string) => /^\s*(\/\/|\/\*|\*)/.test(line);
  const offences = (src: string): string[] =>
    src
      .split('\n')
      .filter((line) => !isComment(line))
      .filter((line) => NEEDLES.some((n) => line.includes(n)) || DATE_TO_LOCALE.test(line));

  it('only lib/format.ts and lib/timeZone.ts turn an instant into wall-clock text', () => {
    const offenders = readSources()
      .filter(([p]) => !ALLOWED.has(p))
      .flatMap(([p, src]) => offences(src).map((line) => `${p}: ${line.trim()}`));
    expect(
      offenders,
      `format through lib/format (formatTimestamp, formatClockTime, …) or lib/timeZone (wallClock):\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('recognises an offence and lets a number through', () => {
    expect(offences('const s = new Date(t).toLocale' + 'TimeString();')).toHaveLength(1);
    expect(offences('const h = d.get' + 'Hours();')).toHaveLength(1);
    expect(offences('const s = new Date(t).toLocale' + 'String();')).toHaveLength(1);
    expect(offences('render: (r) => r.packets.toLocale' + 'String(),')).toHaveLength(0);
    expect(offences('const ms = new Date(x).getTime();')).toHaveLength(0);
    expect(offences('  // rows are in `Date.get' + 'Day()` order')).toHaveLength(0);
  });

  it('finds the sources it is supposed to be reading', () => {
    const files = readSources();
    expect(files.length).toBeGreaterThan(300);
    const own = files.find(([p]) => p === 'lib/format.ts')?.[1] ?? '';
    expect(own.includes('displayTimeZone()')).toBe(true);
  });
});
