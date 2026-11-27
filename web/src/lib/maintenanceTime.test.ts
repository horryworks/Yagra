// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  RECENT_ZONES_MAX,
  addWallMinutes,
  arrowStep,
  clockValue,
  endAfter,
  endBase,
  followStart,
  keptLength,
  NEW_WINDOW_MINUTES,
  newWindowFields,
  nextWholeHour,
  settleEnd,
  stepEnd,
  samePart,
  stepDateText,
  stepMinutes,
  stepTimeOfDayText,
  stepWallText,
  timePart,
  typedTimePart,
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

  it('reads the full-width text a Japanese input method commits', () => {
    expect(parseDateText('２０２６－１１－２０')).toEqual({
      year: 2026,
      month: 11,
      day: 20,
    });
    expect(parseDateText('２０２６／１１／２０')).toEqual({
      year: 2026,
      month: 11,
      day: 20,
    });
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

  it('reads the full-width text a Japanese input method commits', () => {
    expect(parseTimeText('２２：００')).toEqual({ hour: 22, minute: 0 });
    expect(parseTimeText('２２００')).toEqual({ hour: 22, minute: 0 });
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
  endDate: '2026-11-21',
  endTime: '00:00',
  zone: 'America/New_York',
};

describe('windowTimes', () => {
  it('reads the start and the end in the chosen zone', () => {
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

  it('reads the end, and refuses one at or before the start', () => {
    const custom = { ...base, endDate: '', endTime: '' };
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
    const t = windowTimes({ ...f, zone: 'America/New_York' });
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

// ── ADR-261 Inc.6: the ▲▼ on the boxes, and the end that follows the start ─────────────────

const wall = (date: string, time: string) => {
  const d = parseDateText(date);
  const t = parseTimeText(time);
  if (!d || d === 'invalid' || !t || t === 'invalid') throw new Error('bad fixture');
  return { date: d, time: t };
};

describe('stepping a date or a time', () => {
  it('moves a date a day, across a month and a year end', () => {
    const today = wall('2026-11-27', '00:00').date;
    expect(stepDateText('2026-11-30', 1, today)).toBe('2026-12-01');
    expect(stepDateText('2027-01-01', -1, today)).toBe('2026-12-31');
    expect(stepDateText('2028/02/28', 1, today)).toBe('2028-02-29');
  });

  it('fills an empty or unreadable date with the base and does not move it', () => {
    const today = wall('2026-11-27', '00:00').date;
    expect(stepDateText('', 1, today)).toBe('2026-11-27');
    expect(stepDateText('11/30', -1, today)).toBe('2026-11-27');
  });

  it('moves an hour by an hour and keeps the minutes', () => {
    expect(stepMinutes({ hour: 22, minute: 10 }, 'hour', 1)).toBe(60);
    expect(stepMinutes({ hour: 22, minute: 10 }, 'hour', -1)).toBe(-60);
  });

  it('moves a minute to the next quarter in its direction', () => {
    expect(stepMinutes({ hour: 22, minute: 10 }, 'minute', 1)).toBe(5); // 22:15
    expect(stepMinutes({ hour: 22, minute: 10 }, 'minute', -1)).toBe(-10); // 22:00
    expect(stepMinutes({ hour: 22, minute: 0 }, 'minute', 1)).toBe(15);
    expect(stepMinutes({ hour: 22, minute: 0 }, 'minute', -1)).toBe(-15);
    expect(stepMinutes({ hour: 22, minute: 45 }, 'minute', 1)).toBe(15); // 23:00
  });

  it('goes round within the day for a time with no date', () => {
    const base = { hour: 15, minute: 0 };
    expect(stepTimeOfDayText('23:00', 'hour', 1, base)).toBe('00:00');
    expect(stepTimeOfDayText('00:00', 'minute', -1, base)).toBe('23:45');
    expect(stepTimeOfDayText('9:05', 'minute', 1, base)).toBe('09:15');
    expect(stepTimeOfDayText('', 'hour', 1, base)).toBe('15:00');
    expect(stepTimeOfDayText('25:00', 'minute', -1, base)).toBe('15:00');
  });

  it('carries the date when a time beside one steps past midnight', () => {
    const base = wall('2026-11-27', '15:00');
    expect(stepWallText({ date: '2026-11-27', time: '23:00' }, 'hour', 1, base)).toEqual({
      date: '2026-11-28',
      time: '00:00',
    });
    expect(stepWallText({ date: '2026-12-01', time: '00:00' }, 'minute', -1, base)).toEqual({
      date: '2026-11-30',
      time: '23:45',
    });
    expect(stepWallText({ date: '2026-11-27', time: '22:00' }, 'day', 1, base)).toEqual({
      date: '2026-11-28',
      time: '22:00',
    });
  });

  it('fills an empty time with the base and keeps a typed date', () => {
    const base = wall('2026-11-27', '15:00');
    expect(stepWallText({ date: '', time: '' }, 'hour', 1, base)).toEqual({
      date: '2026-11-27',
      time: '15:00',
    });
    expect(stepWallText({ date: '2026-12-24', time: '' }, 'minute', -1, base)).toEqual({
      date: '2026-12-24',
      time: '15:00',
    });
    // A readable time with no date steps from the base's day.
    expect(stepWallText({ date: '', time: '23:30' }, 'hour', 1, base)).toEqual({
      date: '2026-11-28',
      time: '00:30',
    });
  });

  it('starts an empty start box at the next whole hour on the chosen clock', () => {
    // 14:37 in Tokyo is 05:37 UTC.
    const now = Date.parse('2026-11-27T05:37:00Z');
    expect(nextWholeHour(now, 'Asia/Tokyo')).toEqual(wall('2026-11-27', '15:00'));
    expect(nextWholeHour(now, 'UTC')).toEqual(wall('2026-11-27', '06:00'));
    // 23:20 rolls into the next day.
    expect(nextWholeHour(Date.parse('2026-11-27T14:20:00Z'), 'Asia/Tokyo')).toEqual(
      wall('2026-11-28', '00:00'),
    );
  });

  it('starts an empty end box an hour after the start, never on it', () => {
    const now = Date.parse('2026-11-27T05:37:00Z');
    expect(endBase({ date: '2026-11-27', time: '23:30' }, now, 'Asia/Tokyo')).toEqual(
      wall('2026-11-28', '00:30'),
    );
    expect(endBase({ date: '', time: '' }, now, 'Asia/Tokyo')).toEqual(wall('2026-11-27', '16:00'));
  });

  it('adds wall minutes with the date following', () => {
    expect(addWallMinutes(wall('2026-12-31', '23:30'), 45)).toEqual(wall('2027-01-01', '00:15'));
  });

  it('reads the arrow keys and nothing else', () => {
    expect(arrowStep('ArrowUp')).toBe(1);
    expect(arrowStep('ArrowDown')).toBe(-1);
    expect(arrowStep('Enter')).toBeNull();
  });
});

describe('the two boxes of a time', () => {
  it('shows each half with two digits once the time reads, else what was typed', () => {
    expect(timePart('9:05', 0)).toBe('09');
    expect(timePart('9:05', 1)).toBe('05');
    expect(timePart('2200', 1)).toBe('00');
    expect(timePart('25:00', 0)).toBe('25');
    expect(timePart('22:3', 1)).toBe('3');
    expect(timePart('', 0)).toBe('');
  });

  it('keeps the last two digits typed in a box, and brings 00 with a lone hour', () => {
    expect(typedTimePart('2', 0, '', false)).toEqual({ shown: '2', text: '2:00' });
    expect(typedTimePart('22', 0, '2:00', false)).toEqual({ shown: '22', text: '22:00' });
    expect(typedTimePart('223', 0, '22:00', false)).toEqual({ shown: '23', text: '23:00' });
    expect(typedTimePart('3', 1, '22:00', false)).toEqual({ shown: '3', text: '22:3' });
    expect(typedTimePart('30', 1, '22:3', false)).toEqual({ shown: '30', text: '22:30' });
    expect(typedTimePart('', 0, '22:00', false)).toEqual({ shown: '', text: ':00' });
    expect(typedTimePart('', 1, '', false)).toEqual({ shown: '', text: '' });
  });

  it('splits a whole time pasted into one box', () => {
    expect(typedTimePart('22:00', 0, '', true)).toEqual({ shown: '22', text: '22:00' });
    expect(typedTimePart('0930', 0, '', true)).toEqual({ shown: '09', text: '09:30' });
    expect(typedTimePart('0930', 1, '', true)).toEqual({ shown: '30', text: '09:30' });
    // Not a time: the digits go in the box as if typed.
    expect(typedTimePart('7', 0, '', true)).toEqual({ shown: '7', text: '7:00' });
  });

  it('reads the full-width digits and colon a Japanese input method commits', () => {
    expect(typedTimePart('２２', 0, '', false)).toEqual({ shown: '22', text: '22:00' });
    expect(typedTimePart('３０', 1, '22:00', false)).toEqual({ shown: '30', text: '22:30' });
    expect(typedTimePart('２２：３０', 0, '', true)).toEqual({
      shown: '22',
      text: '22:30',
    });
  });

  it('treats 2 and 02 as the same text, so a value coming back does not overwrite typing', () => {
    expect(samePart('2', '02')).toBe(true);
    expect(samePart('', '')).toBe(true);
    expect(samePart('2', '03')).toBe(false);
    expect(samePart('', '00')).toBe(false);
  });
});

describe('the end that follows the start', () => {
  const tz = 'Asia/Tokyo';
  const start = { date: '2026-11-27', time: '22:00' };

  it('keeps the length between a start and an end that read', () => {
    expect(keptLength(start, { date: '2026-11-28', time: '00:30' }, tz)).toBe(150 * 60_000);
    expect(keptLength(start, { date: '2026-11-27', time: '21:00' }, tz)).toBeNull();
    expect(keptLength(start, { date: '', time: '' }, tz)).toBeNull();
    expect(keptLength({ date: '', time: '' }, { date: '2026-11-28', time: '00:30' }, tz)).toBeNull();
  });

  it('moves the end with the start, and leaves it alone when there is nothing to keep', () => {
    const later = { date: '2026-11-27', time: '23:00' };
    expect(followStart(later, 2 * 3_600_000, tz)).toEqual({ date: '2026-11-28', time: '01:00' });
    expect(followStart(later, null, tz)).toBeNull();
    // Half-typed: the end waits until the start reads again.
    expect(followStart({ date: '2026-11-', time: '23:00' }, 2 * 3_600_000, tz)).toBeNull();
  });

  it('fills the end from a shortcut as time passes, across a clock change', () => {
    expect(endAfter(start, 120, tz)).toEqual({ date: '2026-11-28', time: '00:00' });
    expect(endAfter({ date: '', time: '22:00' }, 120, tz)).toBeNull();
    // New York falls back at 02:00 on 1 November 2026: two hours after 00:30 EDT is 01:30 EST.
    expect(endAfter({ date: '2026-11-01', time: '00:30' }, 120, 'America/New_York')).toEqual({
      date: '2026-11-01',
      time: '01:30',
    });
  });
});

// ── ADR-261 Inc.7: a new window opens filled in, and the end is always after the start ──────

describe('a new window', () => {
  it('opens from the next whole hour on the zone clock to an hour after it', () => {
    // 05:20 UTC is 14:20 in Tokyo.
    expect(newWindowFields(Date.parse('2026-11-27T05:20:00Z'), 'Asia/Tokyo')).toEqual({
      startDate: '2026-11-27',
      startTime: '15:00',
      endDate: '2026-11-27',
      endTime: '16:00',
    });
    expect(NEW_WINDOW_MINUTES).toBe(60);
  });

  it('crosses midnight with the date', () => {
    // 14:30 UTC is 23:30 in Tokyo.
    expect(newWindowFields(Date.parse('2026-11-27T14:30:00Z'), 'Asia/Tokyo')).toEqual({
      startDate: '2026-11-28',
      startTime: '00:00',
      endDate: '2026-11-28',
      endTime: '01:00',
    });
  });
});

describe('the end that settles after the start', () => {
  const tz = 'Asia/Tokyo';
  const start = { date: '2026-11-27', time: '22:00' };
  const twoHours = 2 * 3_600_000;

  it('leaves an end that is already after the start alone', () => {
    expect(settleEnd(start, { date: '2026-11-28', time: '00:30' }, twoHours, tz)).toBeNull();
  });

  it('moves an end at or before the start to the first time after it on the same clock', () => {
    // 02:00 typed for a 22:00 start is the next morning.
    expect(settleEnd(start, { date: '2026-11-27', time: '02:00' }, twoHours, tz)).toEqual({
      date: '2026-11-28',
      time: '02:00',
    });
    // A later time on the start's own day stays on it, even from an earlier date.
    expect(settleEnd(start, { date: '2026-11-20', time: '23:00' }, twoHours, tz)).toEqual({
      date: '2026-11-27',
      time: '23:00',
    });
    // The start itself is no window: the same clock a day later.
    expect(settleEnd(start, { date: '2026-11-27', time: '22:00' }, twoHours, tz)).toEqual({
      date: '2026-11-28',
      time: '22:00',
    });
  });

  it('fills an empty end with the start plus the kept length', () => {
    expect(settleEnd(start, { date: '', time: '' }, twoHours, tz)).toEqual({
      date: '2026-11-28',
      time: '00:00',
    });
  });

  it('leaves a half-typed or malformed end, and anything while the start cannot be read', () => {
    expect(settleEnd(start, { date: '2026-11-', time: '02:00' }, twoHours, tz)).toBeNull();
    expect(settleEnd(start, { date: '', time: '02:00' }, twoHours, tz)).toBeNull();
    expect(settleEnd(start, { date: '2026-11-27', time: '' }, twoHours, tz)).toBeNull();
    expect(settleEnd({ date: '', time: '22:00' }, { date: '', time: '' }, twoHours, tz)).toBeNull();
  });

  it('passes over a day on which the clocks skip that time', () => {
    // New York springs forward at 02:00 on 14 March 2027, so 02:30 does not exist that day.
    expect(
      settleEnd(
        { date: '2027-03-13', time: '03:00' },
        { date: '2027-03-13', time: '02:30' },
        twoHours,
        'America/New_York',
      ),
    ).toEqual({ date: '2027-03-15', time: '02:30' });
  });
});

describe('a step on the end', () => {
  const tz = 'Asia/Tokyo';
  const start = { date: '2026-11-27', time: '22:00' };
  const twoHours = 2 * 3_600_000;
  const now = Date.parse('2026-11-27T05:20:00Z');

  it('steps like any end while it stays after the start', () => {
    expect(stepEnd(start, { date: '2026-11-27', time: '22:30' }, 'minute', -1, twoHours, tz, now)).toEqual({
      date: '2026-11-27',
      time: '22:15',
    });
    expect(stepEnd(start, { date: '2026-11-27', time: '23:00' }, 'hour', 1, twoHours, tz, now)).toEqual({
      date: '2026-11-28',
      time: '00:00',
    });
  });

  it('does not take the end to the start or before it', () => {
    expect(stepEnd(start, { date: '2026-11-27', time: '23:00' }, 'hour', -1, twoHours, tz, now)).toBeNull();
    expect(stepEnd(start, { date: '2026-11-27', time: '22:15' }, 'minute', -1, twoHours, tz, now)).toBeNull();
    expect(stepEnd(start, { date: '2026-11-28', time: '02:00' }, 'day', -1, twoHours, tz, now)).toBeNull();
  });

  it('steps from where a just-typed end will settle', () => {
    // 02:00 just typed beside the start's date: one hour up is the next day's 03:00.
    expect(stepEnd(start, { date: '2026-11-27', time: '02:00' }, 'hour', 1, twoHours, tz, now)).toEqual({
      date: '2026-11-28',
      time: '03:00',
    });
  });

  it('fills an empty end with the start plus the kept length and does not move it', () => {
    expect(stepEnd(start, { date: '', time: '' }, 'hour', 1, twoHours, tz, now)).toEqual({
      date: '2026-11-28',
      time: '00:00',
    });
  });
});
