// SPDX-License-Identifier: AGPL-3.0-only
// When config backups are fetched (ADR-210 stage 6): the line above Nodes ▸ Config backups and what
// its dialog may send. A `.ts` because Vitest never loads a `.tsx` (`testing.md`).

import type { ConfigBackupScheduleView } from '../types/api';
import { timeValue } from '../lib/format';
import { wallClock, zoneAbbreviation } from '../lib/timeZone';

/** Where the latest run stands, as the second half of the line. */
export type ScheduleState =
  | { key: 'config.schedule.next'; at: string }
  | { key: 'config.schedule.running'; done: number; total: number }
  | { key: 'config.schedule.runningBare' };

/** The line: the rule (start time and how many at once) and the state. */
export interface ScheduleLine {
  time: string;
  /** The start time's zone, as a device would print it (`JST`, `UTC`). */
  zone: string;
  count: number;
  state: ScheduleState;
}

export function scheduleLine(s: ConfigBackupScheduleView): ScheduleLine {
  const time = timeValue(s.at_hour, s.at_minute);
  let state: ScheduleState;
  if (s.running) {
    // A caller who does not see the whole fleet is not given the counts.
    state =
      s.done != null && s.total != null
        ? { key: 'config.schedule.running', done: s.done, total: s.total }
        : { key: 'config.schedule.runningBare' };
  } else if (s.upcoming_start_at) {
    state = { key: 'config.schedule.next', at: s.upcoming_start_at };
  } else {
    state = { key: 'config.schedule.runningBare' };
  }
  return { time, zone: zoneAbbreviation(s.time_zone || 'UTC'), count: s.concurrency, state };
}

/** The start time the dialog opens with, as a wall time in `zone` — the zone it will save in. A
 *  schedule that never saved a zone is UTC (ADR-221) but is offered in the display zone, so its
 *  stored hour is turned into that zone first: saving only a new count must not move the run. */
export function scheduleDraftTime(
  s: Pick<ConfigBackupScheduleView, 'at_hour' | 'at_minute' | 'time_zone'>,
  zone: string,
  now: number = Date.now(),
): string {
  if (s.time_zone) return timeValue(s.at_hour, s.at_minute);
  const today = new Date(now);
  const instant = Date.UTC(
    today.getUTCFullYear(),
    today.getUTCMonth(),
    today.getUTCDate(),
    s.at_hour,
    s.at_minute,
  );
  const w = wallClock(instant, zone);
  return timeValue(w.hour, w.minute);
}

/** The body the dialog sends, or the key of why it cannot. */
export function scheduleBody(
  time: string,
  concurrency: number,
  max: number,
  timeZone: string,
):
  | {
      ok: true;
      body: { at_hour: number; at_minute: number; concurrency: number; time_zone: string };
    }
  | { ok: false; key: 'config.schedule.err.time' | 'config.schedule.err.count' } {
  const m = /^(\d{1,2}):(\d{2})$/.exec(time.trim());
  const at_hour = m ? Number(m[1]) : NaN;
  const at_minute = m ? Number(m[2]) : NaN;
  if (!(at_hour >= 0 && at_hour <= 23 && at_minute >= 0 && at_minute <= 59)) {
    return { ok: false, key: 'config.schedule.err.time' };
  }
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > max) {
    return { ok: false, key: 'config.schedule.err.count' };
  }
  return { ok: true, body: { at_hour, at_minute, concurrency, time_zone: timeZone } };
}
