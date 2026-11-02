// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  daysText,
  heldFireFor,
  hoursText,
  minutesToTime,
  scheduleBodyFrom,
  scheduleDraftFrom,
  timeToMinutes,
} from './deliveryHours';
import type { HeldNotification } from '../types/api';
import type { TFunction } from 'i18next';

/** The last segment of a key, so `reports:weekday.mon` reads `mon`. */
const t = ((k: string) => k.split('.').pop() ?? k) as unknown as TFunction;

describe('delivery hours', () => {
  it('turns minutes into a time of day and back', () => {
    expect(minutesToTime(510)).toBe('08:30');
    expect(minutesToTime(1040)).toBe('17:20');
    expect(timeToMinutes('08:30')).toBe(510);
    expect(timeToMinutes('8:30')).toBe(510);
    expect(timeToMinutes('24:00')).toBeNull();
    expect(timeToMinutes('noon')).toBeNull();
  });

  it('says a run of days as a range and the rest as a list', () => {
    expect(daysText(t, [1, 2, 3, 4, 5])).toBe('mon–fri');
    expect(daysText(t, [6, 7])).toBe('sat, sun');
    expect(daysText(t, [5, 1, 3])).toBe('mon, wed, fri');
  });

  it('says an equal start and end as the whole day', () => {
    expect(hoursText({ start_minute: 0, end_minute: 0 })).toBe('00:00–24:00');
    expect(hoursText({ start_minute: 1320, end_minute: 360 })).toBe('22:00–06:00');
  });

  it('starts a new schedule at office hours on weekdays', () => {
    const d = scheduleDraftFrom();
    expect(d.days).toEqual([1, 2, 3, 4, 5]);
    expect([d.start, d.end]).toEqual(['08:30', '17:20']);
  });

  it('refuses what the server refuses, before sending', () => {
    const ok = { ...scheduleDraftFrom(), name: 'office', labels: ['office'] };
    expect('body' in scheduleBodyFrom(ok, true)).toBe(true);
    expect(scheduleBodyFrom({ ...ok, name: ' ' }, true)).toEqual({ error: 'name' });
    expect(scheduleBodyFrom({ ...ok, days: [] }, true)).toEqual({ error: 'days' });
    expect(scheduleBodyFrom({ ...ok, end: '25:00' }, true)).toEqual({ error: 'time' });
    expect(scheduleBodyFrom({ ...ok, labels: [' '] }, true)).toEqual({ error: 'target' });
    const r = scheduleBodyFrom({ ...ok, days: [5, 1] }, false);
    expect('body' in r && r.body.days).toEqual([1, 5]);
    expect('body' in r && r.body.enabled).toBe(false);
  });

  it('marks an open alert only for its own held fire', () => {
    const held: HeldNotification[] = [
      {
        node_id: 'n1',
        check: 'c1',
        metric: 'icmp_rtt_ms',
        severity: 'critical',
        event: 'fire',
        held_at: '2026-11-02T20:00:00Z',
        release_at: '2026-11-03T08:30:00Z',
      },
      {
        node_id: 'n2',
        check: 'c2',
        metric: 'icmp_rtt_ms',
        severity: 'critical',
        event: 'resolve',
        held_at: '2026-11-02T20:00:00Z',
        release_at: null,
      },
    ];
    expect(heldFireFor({ check: 'c1' }, held, 'n1')?.release_at).toBe('2026-11-03T08:30:00Z');
    expect(heldFireFor({ check: 'c1' }, held, 'n9')).toBeNull();
    expect(heldFireFor({ check: 'c2' }, held, 'n2')).toBeNull();
    expect(heldFireFor({ check: 'c1' }, held, null)).toBeNull();
  });
});
