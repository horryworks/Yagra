// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Alerts ▸ Notification delivery's grouping section (ADR-236): what each level
// would do to the outage open now, and the last seven days replayed at a level.

import { GROUPING_LEVELS, type AlertGroupingConfig, type GroupingLevel } from '../types/api';

/** How many of the nodes down now would page on their own at `level`. The server lists the counts
 *  weakest level first, in `GROUPING_LEVELS` order; a list that came back short answers "all of
 *  them", which is what grouping nothing means. */
export function notifiedAt(cfg: AlertGroupingConfig, level: GroupingLevel): number {
  return cfg.now.notified[GROUPING_LEVELS.indexOf(level)] ?? cfg.now.down;
}

/** What the last seven days would have paged at `level`, sending at once and waiting, or `null`
 *  when the server could not replay them (the line is left out rather than shown as zeros). */
export interface ReplayLine {
  fired: number;
  atOnce: number;
  waiting: number;
  /** The longest wait of a fire that waited and then paged, seconds; `null` when none did. */
  maxDelaySecs: number | null;
  truncated: boolean;
}

export function replayAt(cfg: AlertGroupingConfig, level: GroupingLevel): ReplayLine | null {
  const r = cfg.last_7_days;
  if (!r) return null;
  const l = r.levels.find((x) => x.level === level);
  return {
    fired: r.fired,
    // A level the server did not answer for pages everything, as grouping nothing does.
    atOnce: l?.notified ?? r.fired,
    waiting: l?.notified_waiting ?? r.fired,
    maxDelaySecs: l?.delay_max_secs ?? null,
    truncated: r.truncated,
  };
}

/** Whether a string the switch handed back is a level. */
export function isGroupingLevel(v: string): v is GroupingLevel {
  return (GROUPING_LEVELS as readonly string[]).includes(v);
}
