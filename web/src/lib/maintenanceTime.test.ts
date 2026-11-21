// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  RECENT_ZONES_MAX,
  clockValue,
  joinDateTime,
  splitDateTime,
  matchZones,
  offsetQuery,
  parseDateText,
  parseTimeText,
  pushRecentZone,
  rangeLine,
  requesterClock,
  targetZone,
  tonightStart,
  wallLine,
  weekdayOf,
  windowFields,
  windowName,
  windowTimes,
  type WindowTimeInput,
} from './maintenanceTime';
import { zoneAbbreviation } from './timeZone';

describe('parseDateText', () => {
  it('reads a year-first date with either separator', () => {
    expect(parseDateText('2026-11-20')).toEqual({ year: 2026, month: 11, day: 20 });
    expect(parseDateText(' 2026/11/20 ')).toEqual({ year: 2026, month: 11, day: 20 });
    expect(parseDateText('2026/1/5')).toEqual({ year: 2026, month: 1, day: 5 });
  });

  it('is empty for a blank box', () => {
    expect(parseDateText('  ')).toBeNull();
  });

  it('refuses a date with no year, because the month and the day cannot be told apart', () => {
    expect(parseDateText('11/20')).toBe('invalid');
    expect(parseDateText('11/20/2026')).toBe('invalid');
    expect(parseDateText('20/11/2026')).toBe('invalid');
  });

  it('refuses a day the month does not have, leap years included', () => {
    expect(parseDateText('2026-02-29')).toBe('invalid');
    expect(parseDateText('2028-02-29')).toEqual({ year: 2028, month: 2, day: 29 });
    expect(parseDateText('2026-04-31')).toBe('invalid');
    expect(parseDateText('2026-13-01')).toBe('invalid');
    expect(parseDateText('2026-00-10')).toBe('invalid');
  });
});

describe('parseTimeText', () => {
  it('reads a 24-hour time with or without the colon', () => {
    expect(parseTimeText('22:00')).toEqual({ hour: 22, minute: 0 });
    expect(parseTimeText('2200')).toEqual({ hour: 22, minute: 0 });
    expect(parseTimeText('9:05')).toEqual({ hour: 9, minute: 5 });
    expect(parseTimeText('905')).toEqual({ hour: 9, minute: 5 });
    expect(parseTimeText('0:00')).toEqual({ hour: 0, minute: 0 });
  });

  it('refuses AM/PM and out-of-range values', () => {
    expect(parseTimeText('10pm')).toBe('invalid');
    expect(parseTimeText('10:00 PM')).toBe('invalid');
    expect(parseTimeText('24:00')).toBe('invalid');
    expect(parseTimeText('12:60')).toBe('invalid');
    expect(parseTimeText('22')).toBe('invalid');
  });
});

const base: WindowTimeInput = {
  startDate: '2026-11-20',
  startTime: '22:00',
  length: '2h',
  endDate: '',
  endTime: '',
  zone: 'America/New_York',
};

describe('windowTimes', () => {
  it('reads the start in the chosen zone and adds the length', () => {
    const r = windowTimes(base);
    // 22:00 EST (UTC-5) on 20 November is 03:00 UTC on the 21st.
    expect(r).toEqual({
      kind: 'ok',
      startMs: Date.parse('2026-11-21T03:00:00Z'),
      endMs: Date.parse('2026-11-21T05:00:00Z'),
    });
  });

  it('reads the same wall time differently in another zone', () => {
    const r = windowTimes({ ...base, zone: 'Asia/Tokyo' });
    expect(r).toMatchObject({ kind: 'ok', startMs: Date.parse('2026-11-20T13:00:00Z') });
  });

  it('is incomplete until both start fields are filled', () => {
    expect(windowTimes({ ...base, startTime: '' })).toEqual({ kind: 'incomplete' });
  });

  it('names the field that cannot be read', () => {
    expect(windowTimes({ ...base, startDate: '11/20' })).toEqual({
      kind: 'invalid',
      field: 'startDate',
    });
    expect(windowTimes({ ...base, startTime: '10pm' })).toEqual({
      kind: 'invalid',
      field: 'startTime',
    });
  });

  it('refuses a start the clocks skip, rather than moving it', () => {
    // New York springs forward at 02:00 on 8 March 2026: 02:30 does not happen.
    expect(windowTimes({ ...base, startDate: '2026-03-08', startTime: '02:30' })).toEqual({
      kind: 'gap',
      field: 'start',
    });
  });

  it('takes the first of a time that happens twice', () => {
    // New York falls back at 02:00 on 1 November 2026: 01:30 happens in EDT, then in EST.
    const r = windowTimes({ ...base, startDate: '2026-11-01', startTime: '01:30' });
    expect(r).toMatchObject({ kind: 'ok', startMs: Date.parse('2026-11-01T05:30:00Z') });
  });

  it('reads a picked end, and refuses one at or before the start', () => {
    const custom = { ...base, length: 'custom' as const };
    expect(windowTimes(custom)).toEqual({ kind: 'incomplete' });
    expect(windowTimes({ ...custom, endDate: '2026-11-21', endTime: '01:30' })).toEqual({
      kind: 'ok',
      startMs: Date.parse('2026-11-21T03:00:00Z'),
      endMs: Date.parse('2026-11-21T06:30:00Z'),
    });
    expect(windowTimes({ ...custom, endDate: '2026-11-20', endTime: '22:00' })).toEqual({
      kind: 'backwards',
    });
    expect(windowTimes({ ...custom, endDate: '2026-11-21', endTime: '7pm' })).toEqual({
      kind: 'invalid',
      field: 'endTime',
    });
  });
});

describe('tonightStart', () => {
  it('is today at 22:00 in the zone while that is still ahead', () => {
    // 15:00 in Tokyo.
    const now = Date.parse('2026-11-20T06:00:00Z');
    expect(tonightStart(now, 'Asia/Tokyo')).toEqual({
      date: { year: 2026, month: 11, day: 20 },
      time: { hour: 22, minute: 0 },
    });
  });

  it('moves to tomorrow once 22:00 has begun, across a month end', () => {
    // 23:00 on 30 November in Tokyo.
    const now = Date.parse('2026-11-30T14:00:00Z');
    expect(tonightStart(now, 'Asia/Tokyo').date).toEqual({ year: 2026, month: 12, day: 1 });
  });

  it('reads "today" on the chosen clock, not the viewer’s', () => {
    // 01:00 on the 21st in Tokyo is still 11:00 on the 20th in New York.
    const now = Date.parse('2026-11-20T16:00:00Z');
    expect(tonightStart(now, 'America/New_York').date).toEqual({ year: 2026, month: 11, day: 20 });
  });
});

describe('the lines the dialog shows', () => {
  it('names the end on the chosen clock', () => {
    expect(wallLine(Date.parse('2026-11-21T05:00:00Z'), 'America/New_York')).toBe(
      '2026-11-21 Sat 00:00 EST',
    );
  });

  it('shows the viewer’s own range, with the date on the end only when it is another day', () => {
    const start = Date.parse('2026-11-21T03:00:00Z');
    // The abbreviation is whatever this runtime's Intl calls Tokyo (`JST` or `GMT+9`).
    const jst = zoneAbbreviation('Asia/Tokyo', start);
    expect(rangeLine(start, start + 2 * 3_600_000, 'Asia/Tokyo')).toBe(
      `11-21 Sat 12:00 – 14:00 ${jst}`,
    );
    expect(rangeLine(start, start + 14 * 3_600_000, 'Asia/Tokyo')).toBe(
      `11-21 Sat 12:00 – 11-22 Sun 02:00 ${jst}`,
    );
  });

  it('gives a typed date its weekday, and none to an unreadable one', () => {
    expect(weekdayOf('2026-11-20')).toBe('Fri');
    expect(weekdayOf('11/20')).toBe('');
  });
});

describe('zone search', () => {
  const zones = ['America/New_York', 'Asia/Kolkata', 'Asia/Tokyo', 'Europe/London', 'UTC'];
  const now = Date.parse('2026-11-20T00:00:00Z');

  it('finds a zone by the words of its name', () => {
    expect(matchZones('new york', zones, now)).toEqual(['America/New_York']);
    expect(matchZones('TOKYO', zones, now)).toEqual(['Asia/Tokyo']);
    expect(matchZones('asia', zones, now)).toEqual(['Asia/Kolkata', 'Asia/Tokyo']);
    expect(matchZones('', zones, now)).toEqual(zones);
  });

  it('finds a zone by its offset at this moment', () => {
    expect(matchZones('utc+9', zones, now)).toEqual(['Asia/Tokyo']);
    expect(matchZones('UTC+05:30', zones, now)).toEqual(['Asia/Kolkata']);
    expect(matchZones('gmt-5', zones, now)).toEqual(['America/New_York']);
    expect(matchZones('+0000', zones, now)).toEqual(['Europe/London', 'UTC']);
  });

  it('reads only real offsets as offsets', () => {
    expect(offsetQuery('utc+5')).toBe('+0500');
    expect(offsetQuery('-3:30')).toBe('-0330');
    expect(offsetQuery('utc+15')).toBeNull();
    expect(offsetQuery('utc')).toBeNull();
    expect(offsetQuery('tokyo')).toBeNull();
  });

  it('keeps the latest zone first and remembers only a few', () => {
    let recent: string[] = [];
    for (const z of ['A', 'B', 'C', 'B', 'D', 'E', 'F', 'G']) recent = pushRecentZone(recent, z);
    expect(recent).toEqual(['G', 'F', 'E', 'D', 'B']);
    expect(recent).toHaveLength(RECENT_ZONES_MAX);
  });
});

describe('targetZone', () => {
  const zoneOf = (id: string) =>
    ({ ny: 'America/New_York', ny2: 'America/New_York', tk: 'Asia/Tokyo', none: null })[id];

  it('is the zone every target shares', () => {
    expect(targetZone(['ny'], zoneOf)).toBe('America/New_York');
    expect(targetZone(['ny', 'ny2'], zoneOf)).toBe('America/New_York');
  });

  it('is nothing when the targets disagree, or one has no zone or no folder', () => {
    expect(targetZone(['ny', 'tk'], zoneOf)).toBeNull();
    expect(targetZone(['ny', 'none'], zoneOf)).toBeNull();
    expect(targetZone(['ny', null], zoneOf)).toBeNull();
    expect(targetZone([], zoneOf)).toBeNull();
  });
});

describe('the value shapes of the inputs these boxes replace', () => {
  it('gives a time as <input type="time"> did, or nothing', () => {
    expect(clockValue('2200')).toBe('22:00');
    expect(clockValue('9:05')).toBe('09:05');
    expect(clockValue('')).toBe('');
    expect(clockValue('10pm')).toBe('');
  });

  it('splits and joins a datetime-local value', () => {
    expect(splitDateTime('2026-11-20T22:00')).toEqual({ date: '2026-11-20', time: '22:00' });
    expect(splitDateTime('')).toEqual({ date: '', time: '' });
    expect(joinDateTime('2026/1/5', '930')).toBe('2026-01-05T09:30');
    expect(joinDateTime('2026-11-20', '')).toBe('');
    expect(joinDateTime('11/20', '22:00')).toBe('');
  });
});

describe('requesterClock', () => {
  const start = '2026-11-21T03:00:00Z';
  const end = '2026-11-21T05:00:00Z';

  it('shows the times on the clock the window was asked in', () => {
    expect(requesterClock(start, end, 'America/New_York', 'Asia/Tokyo')).toBe('22:00–00:00 EST');
  });

  it('says nothing when no zone was recorded, or it is the viewer’s own', () => {
    expect(requesterClock(start, end, null, 'Asia/Tokyo')).toBeNull();
    expect(requesterClock(start, end, 'Asia/Tokyo', 'Asia/Tokyo')).toBeNull();
    expect(requesterClock('nonsense', end, 'America/New_York', 'Asia/Tokyo')).toBeNull();
  });
});

describe('windowName', () => {
  it('saves what was typed, or the default when nothing was', () => {
    expect(windowName('  Firmware CHG-1 ', 'Maintenance — sw-01')).toBe('Firmware CHG-1');
    expect(windowName('   ', 'Maintenance — sw-01')).toBe('Maintenance — sw-01');
  });
});

describe('windowFields', () => {
  it('reads a stored window back on the clock it was asked in, end as a picked end', () => {
    expect(
      windowFields('2030-01-02T13:00:00+00:00', '2030-01-02T17:30:00+00:00', 'Asia/Tokyo'),
    ).toEqual({
      startDate: '2030-01-02',
      startTime: '22:00',
      endDate: '2030-01-03',
      endTime: '02:30',
    });
  });

  it('round-trips through windowTimes to the same instants', () => {
    const f = windowFields('2030-03-09T06:15:00Z', '2030-03-09T09:00:00Z', 'America/New_York');
    const t = windowTimes({ ...f, length: 'custom', zone: 'America/New_York' });
    expect(t).toEqual({
      kind: 'ok',
      startMs: Date.parse('2030-03-09T06:15:00Z'),
      endMs: Date.parse('2030-03-09T09:00:00Z'),
    });
  });

  it('leaves an unreadable bound empty rather than inventing one', () => {
    expect(windowFields('soon', '2030-01-01T00:00:00Z', 'UTC')).toEqual({
      startDate: '',
      startTime: '',
      endDate: '2030-01-01',
      endTime: '00:00',
    });
  });
});
