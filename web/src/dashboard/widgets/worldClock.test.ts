// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_ZONES,
  LOCAL_ZONE,
  MAX_ZONES,
  addZone,
  cityName,
  moveZone,
  readClockSettings,
  readZone,
  realTimeZone,
  resolveTimeZone,
} from './worldClock';

const KNOWN = ['UTC', 'Asia/Tokyo', 'America/New_York', 'Europe/London'];
const z = (tz: string, label = '') => ({ tz, label });

describe('readClockSettings', () => {
  it('gives a card with no settings the viewer and UTC, seconds on, 24-hour', () => {
    expect(readClockSettings({})).toEqual({
      zones: DEFAULT_ZONES,
      hour12: false,
      seconds: true,
    });
  });

  it('keeps an emptied card empty rather than restoring the defaults', () => {
    expect(readClockSettings({ zones: [] }).zones).toEqual([]);
  });

  it('drops malformed, unknown and repeated zones, and stops at the cap', () => {
    const zones = [
      z('Asia/Tokyo', 'HQ'),
      'Asia/Tokyo',
      { tz: 42 },
      z('Mars/Base'),
      z('Asia/Tokyo', 'again'),
      { tz: 'UTC', label: 7 },
      z(LOCAL_ZONE),
      ...KNOWN.slice(1).map((tz) => z(tz)),
      ...['Asia/Seoul', 'Europe/Paris', 'Asia/Dubai', 'America/Chicago', 'Asia/Singapore'].map((tz) =>
        z(tz),
      ),
    ];
    const read = readClockSettings({ zones, hour12: true, seconds: false });
    expect(read.zones.length).toBe(MAX_ZONES);
    expect(read.zones.slice(0, 5)).toEqual([
      z('Asia/Tokyo', 'HQ'),
      z('UTC'),
      z(LOCAL_ZONE),
      z('America/New_York'),
      z('Europe/London'),
    ]);
    expect(read.hour12).toBe(true);
    expect(read.seconds).toBe(false);
  });
});

describe('resolveTimeZone and addZone', () => {
  it('matches a listed name ignoring case and stores its spelling', () => {
    expect(resolveTimeZone(' asia/tokyo ', 'Local', KNOWN)).toBe('Asia/Tokyo');
  });

  it('reads the local word in either language as the viewer zone', () => {
    expect(resolveTimeZone('local', 'ローカル', KNOWN)).toBe(LOCAL_ZONE);
    expect(resolveTimeZone('ローカル', 'ローカル', KNOWN)).toBe(LOCAL_ZONE);
  });

  it('accepts a name Intl knows but the list lacks, and refuses one it does not', () => {
    expect(resolveTimeZone('Europe/Paris', 'Local', KNOWN)).toBe('Europe/Paris');
    expect(resolveTimeZone('Mars/Base', 'Local', KNOWN)).toBeNull();
    expect(resolveTimeZone('   ', 'Local', KNOWN)).toBeNull();
  });

  it('says why a zone was refused', () => {
    expect(addZone([], 'Mars/Base', 'Local', KNOWN)).toEqual({ ok: false, reason: 'unknown' });
    expect(addZone([z('Asia/Tokyo')], 'ASIA/TOKYO', 'Local', KNOWN)).toEqual({
      ok: false,
      reason: 'duplicate',
    });
    const full = Array.from({ length: MAX_ZONES }, (_, i) => z(`Etc/GMT+${i + 1}`));
    expect(addZone(full, 'UTC', 'Local', KNOWN)).toEqual({ ok: false, reason: 'full' });
  });

  it('appends an accepted zone with no label', () => {
    expect(addZone([z('UTC')], 'asia/tokyo', 'Local', KNOWN)).toEqual({
      ok: true,
      zones: [z('UTC'), z('Asia/Tokyo')],
    });
  });
});

describe('moveZone and cityName', () => {
  it('swaps neighbours and ignores a move off either end', () => {
    const zs = [z('A'), z('B'), z('C')];
    expect(moveZone(zs, 1, -1).map((x) => x.tz)).toEqual(['B', 'A', 'C']);
    expect(moveZone(zs, 1, 1).map((x) => x.tz)).toEqual(['A', 'C', 'B']);
    expect(moveZone(zs, 0, -1)).toEqual(zs);
    expect(moveZone(zs, 2, 1)).toEqual(zs);
  });

  it('names a zone by its city, and leaves the local one to the caller', () => {
    expect(cityName('America/New_York')).toBe('New York');
    expect(cityName('America/Argentina/Buenos_Aires')).toBe('Buenos Aires');
    expect(cityName('UTC')).toBe('UTC');
    expect(cityName(LOCAL_ZONE)).toBeNull();
    expect(realTimeZone(LOCAL_ZONE, 'Asia/Tokyo')).toBe('Asia/Tokyo');
    expect(realTimeZone('UTC', 'Asia/Tokyo')).toBe('UTC');
  });
});

describe('readZone', () => {
  // 2026-11-03 12:46:07 UTC — after both US and EU daylight saving ended.
  const now = new Date(Date.UTC(2026, 10, 3, 12, 46, 7, 500));

  it('reads the wall clock and offset of each zone', () => {
    expect(readZone('Asia/Tokyo', now, false, 'UTC')).toEqual({
      hh: '21',
      mm: '46',
      ss: '07',
      dayPeriod: '',
      night: true,
      dayShift: 0,
      offset: 'UTC+9',
    });
    expect(readZone('UTC', now, false, 'UTC').offset).toBe('UTC');
    expect(readZone('America/New_York', now, false, 'UTC').offset).toBe('UTC-5');
    expect(readZone('Asia/Kolkata', now, false, 'UTC').offset).toBe('UTC+5:30');
    expect(readZone('America/St_Johns', now, false, 'UTC').offset).toBe('UTC-3:30');
  });

  it('follows daylight saving: London is UTC+1 in July', () => {
    const july = new Date(Date.UTC(2026, 6, 1, 12, 0, 0));
    expect(readZone('Europe/London', july, false, 'UTC').offset).toBe('UTC+1');
  });

  it('marks the day before and after against the viewer', () => {
    // 01:30 UTC: still the 2nd in Los Angeles, already 10:30 on the 3rd in Tokyo.
    const early = new Date(Date.UTC(2026, 10, 3, 1, 30, 0));
    expect(readZone('America/Los_Angeles', early, false, 'Asia/Tokyo').dayShift).toBe(-1);
    expect(readZone('Asia/Tokyo', early, false, 'America/Los_Angeles').dayShift).toBe(1);
    expect(readZone('Asia/Tokyo', early, false, 'Asia/Tokyo').dayShift).toBe(0);
  });

  it('writes the 12-hour cycle with 12 for noon and midnight', () => {
    expect(readZone('Asia/Tokyo', now, true, 'UTC')).toMatchObject({ hh: '09', dayPeriod: 'PM' });
    const midnight = new Date(Date.UTC(2026, 10, 3, 0, 5, 0));
    expect(readZone('UTC', midnight, true, 'UTC')).toMatchObject({ hh: '12', dayPeriod: 'AM' });
    expect(readZone('UTC', midnight, false, 'UTC')).toMatchObject({ hh: '00', dayPeriod: '' });
    const noon = new Date(Date.UTC(2026, 10, 3, 12, 0, 0));
    expect(readZone('UTC', noon, true, 'UTC')).toMatchObject({ hh: '12', dayPeriod: 'PM' });
  });

  it('calls 06:00 to 17:59 day and the rest night', () => {
    const at = (h: number, m = 0) => readZone('UTC', new Date(Date.UTC(2026, 10, 3, h, m)), false, 'UTC');
    expect(at(5, 59).night).toBe(true);
    expect(at(6).night).toBe(false);
    expect(at(17, 59).night).toBe(false);
    expect(at(18).night).toBe(true);
  });
});
