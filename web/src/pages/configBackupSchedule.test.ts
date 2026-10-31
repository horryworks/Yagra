// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ConfigBackupScheduleView } from '../types/api';
import { autoSaveBody, scheduleBody, scheduleLine } from './configBackupSchedule';

const base: ConfigBackupScheduleView = {
  at_hour: 2,
  at_minute: 0,
  concurrency: 1,
  max_concurrency: 8,
  running: false,
  run_started_at: null,
  run_finished_at: null,
  upcoming_start_at: '2026-10-31T02:00:00Z',
  done: null,
  total: null,
  auto_save: { enabled: false, after_hours: 24 },
};

describe('scheduleLine', () => {
  it('says the rule and when the next run starts', () => {
    expect(scheduleLine(base)).toEqual({
      time: '02:00',
      zone: 'UTC',
      count: 1,
      state: { key: 'config.schedule.next', at: '2026-10-31T02:00:00Z' },
    });
  });

  it('says how far a run has got', () => {
    const s = { ...base, running: true, upcoming_start_at: null, done: 37, total: 120, concurrency: 3 };
    expect(scheduleLine(s)).toEqual({
      time: '02:00',
      zone: 'UTC',
      count: 3,
      state: { key: 'config.schedule.running', done: 37, total: 120 },
    });
  });

  it('gives a caller without the counts no numbers', () => {
    const s = { ...base, running: true, upcoming_start_at: null };
    expect(scheduleLine(s).state).toEqual({ key: 'config.schedule.runningBare' });
  });
});

describe('scheduleBody', () => {
  it('sends what the time field holds', () => {
    expect(scheduleBody('17:05', 2, 8, 'Asia/Tokyo')).toEqual({
      ok: true,
      body: { at_hour: 17, at_minute: 5, concurrency: 2, time_zone: 'Asia/Tokyo' },
    });
  });

  it('refuses a time or a count the server would refuse', () => {
    expect(scheduleBody('', 1, 8, 'UTC')).toEqual({ ok: false, key: 'config.schedule.err.time' });
    expect(scheduleBody('24:00', 1, 8, 'UTC')).toEqual({ ok: false, key: 'config.schedule.err.time' });
    expect(scheduleBody('02:60', 1, 8, 'UTC')).toEqual({ ok: false, key: 'config.schedule.err.time' });
    expect(scheduleBody('02:00', 0, 8, 'UTC')).toEqual({ ok: false, key: 'config.schedule.err.count' });
    expect(scheduleBody('02:00', 9, 8, 'UTC')).toEqual({ ok: false, key: 'config.schedule.err.count' });
  });
});

describe('automatic save (ADR-222)', () => {
  it('sends a whole number of hours from 1 to 720 and refuses anything else', () => {
    expect(autoSaveBody(true, ' 24 ')).toEqual({ ok: true, body: { enabled: true, after_hours: 24 } });
    expect(autoSaveBody(false, '720')).toEqual({
      ok: true,
      body: { enabled: false, after_hours: 720 },
    });
    for (const bad of ['0', '721', '1.5', '', 'a', '-3']) {
      expect(autoSaveBody(true, bad)).toEqual({ ok: false, key: 'config.autoSave.err.hours' });
    }
  });
});
