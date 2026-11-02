// SPDX-License-Identifier: AGPL-3.0-only
// The availability graph's judgement (ADR-223): which bars a period has, where each one starts in
// the display zone, and how much of each bar was down, in maintenance, not recorded or up. The
// server returns spans over the whole period; cutting them into bars happens here because the bars
// follow the zone the operator picked on the top-bar clock (ADR-221), which core does not know.

import { displayTimeZone, wallClock, zonedWallToMs } from '../../lib/timeZone';
import type { RangeToken } from '../../lib/filterPresets';
import type { AvailabilitySpan as Span, NodeAvailability } from '../../types/api';

// The server's `period` values, and range tokens, so their labels are the shared ones.
export const AVAILABILITY_PERIODS = ['24h', '7d', '30d', '90d'] as const satisfies readonly (RangeToken &
  NodeAvailability['period'])[];
export type AvailabilityPeriod = (typeof AVAILABILITY_PERIODS)[number];
export const DEFAULT_AVAILABILITY_PERIOD: AvailabilityPeriod = '30d';

/** How each period is cut: `count` bars of `minutes` each, aligned to the display zone's clock. */
const BARS: Record<AvailabilityPeriod, { count: number; minutes: number }> = {
  '24h': { count: 48, minutes: 30 },
  '7d': { count: 42, minutes: 240 },
  '30d': { count: 30, minutes: 1440 },
  '90d': { count: 90, minutes: 1440 },
};

/** Whether a bar is a whole day (its label is a date) or part of one (its label is a time). */
export function barsAreDays(period: AvailabilityPeriod): boolean {
  return BARS[period].minutes >= 1440;
}

/** One bar: its span, and how many seconds of the part that has already happened were each kind. */
export interface AvailabilityBar {
  fromMs: number;
  toMs: number;
  /** Seconds of this bar that are in the past — the last bar is usually only partly so. */
  elapsedSecs: number;
  upSecs: number;
  downSecs: number;
  maintenanceSecs: number;
  noRecordSecs: number;
  /** Down stretches that touch this bar. A monitoring gap can split one outage into two. */
  downCount: number;
}

/** The instant `minutes`-aligned wall time `w` minus `back` bars names in `tz`. */
function barStart(
  w: { year: number; month: number; day: number; minutesOfDay: number },
  minutes: number,
  back: number,
  tz: string,
): number {
  if (minutes >= 1440) {
    return zonedWallToMs(w.year, w.month, w.day - back * (minutes / 1440), 0, 0, tz);
  }
  const m = w.minutesOfDay - back * minutes;
  return zonedWallToMs(w.year, w.month, w.day, 0, m, tz);
}

/** The bars' boundaries, oldest first: the last bar holds `nowMs`. */
export function barBounds(
  period: AvailabilityPeriod,
  nowMs: number,
  tz: string = displayTimeZone(),
): { fromMs: number; toMs: number }[] {
  const { count, minutes } = BARS[period];
  const w = wallClock(nowMs, tz);
  const step = Math.min(minutes, 1440);
  const aligned = {
    year: w.year,
    month: w.month,
    day: w.day,
    minutesOfDay: Math.floor((w.hour * 60 + w.minute) / step) * step,
  };
  const out: { fromMs: number; toMs: number }[] = [];
  for (let i = count - 1; i >= 0; i--) {
    out.push({
      fromMs: barStart(aligned, minutes, i, tz),
      toMs: barStart(aligned, minutes, i - 1, tz),
    });
  }
  return out;
}

const overlap = (a0: number, a1: number, b0: number, b1: number) =>
  Math.max(0, Math.min(a1, b1) - Math.max(a0, b0));

/** Cut the server's spans into bars. Time before `truncated_before_unix_ms` that no span covers is
 *  not recorded, never up — those spans were dropped, not absent. */
export function availabilityBars(
  a: NodeAvailability,
  period: AvailabilityPeriod,
  tz: string = displayTimeZone(),
): AvailabilityBar[] {
  const now = a.to_unix_ms;
  const spans: Span[] = [...a.spans];
  if (a.truncated_before_unix_ms != null && a.truncated_before_unix_ms > a.from_unix_ms) {
    spans.unshift({
      kind: 'no_record',
      from_unix_ms: a.from_unix_ms,
      to_unix_ms: a.truncated_before_unix_ms,
    });
  }
  return barBounds(period, now, tz).map(({ fromMs, toMs }) => {
    // Only the part of the bar inside the answered period counts: the future, and anything older
    // than the period, is neither up nor down.
    const start = Math.max(fromMs, a.from_unix_ms);
    const end = Math.min(toMs, now);
    const ms = { down: 0, maintenance: 0, no_record: 0 };
    let downCount = 0;
    for (const s of spans) {
      const o = overlap(start, end, s.from_unix_ms, s.to_unix_ms);
      if (o <= 0) continue;
      ms[s.kind] += o;
      if (s.kind === 'down') downCount += 1;
    }
    const elapsedMs = Math.max(0, end - start);
    const secs = (v: number) => Math.round(v / 1000);
    return {
      fromMs,
      toMs,
      elapsedSecs: secs(elapsedMs),
      upSecs: secs(Math.max(0, elapsedMs - ms.down - ms.maintenance - ms.no_record)),
      downSecs: secs(ms.down),
      maintenanceSecs: secs(ms.maintenance),
      noRecordSecs: secs(ms.no_record),
      downCount,
    };
  });
}

/** One bar's segments, bottom to top, as shares of the bar's height. Up fills what the others
 *  leave, so the drawing never sums past 100%. A bar with nothing elapsed has no segments. */
export function barSegments(
  bar: AvailabilityBar,
): { kind: 'up' | 'maintenance' | 'down' | 'no_record'; pct: number }[] {
  if (bar.elapsedSecs <= 0) return [];
  const pct = (s: number) => (s / bar.elapsedSecs) * 100;
  return (
    [
      { kind: 'up', pct: pct(bar.upSecs) },
      { kind: 'maintenance', pct: pct(bar.maintenanceSecs) },
      { kind: 'down', pct: pct(bar.downSecs) },
      { kind: 'no_record', pct: pct(bar.noRecordSecs) },
    ] as const
  ).filter((s) => s.pct > 0);
}

/** The headline share: up ÷ (up + down), at two decimals, or null when there is neither. `100`
 *  only when nothing was down — 99.996% must not read as a clean period. */
export function formatAvailabilityPct(pct: number | null | undefined, downSecs: number): string | null {
  if (pct == null) return null;
  if (downSecs === 0) return '100%';
  return `${Math.min(pct, 99.99).toFixed(2)}%`;
}

/** Which bar a point `x` pixels across a strip `width` wide falls in — the whole strip is the
 *  touch target, since a 90-bar strip on a phone leaves each bar about three pixels wide. */
export function barIndexAt(x: number, width: number, count: number): number | null {
  if (width <= 0 || count <= 0 || x < 0 || x > width) return null;
  return Math.min(count - 1, Math.floor((x / width) * count));
}

/** Where an arrow key moves the selected bar; `null` for a key the strip does not take. Nothing
 *  selected yet starts from the newest bar, the one an operator asks about first. */
export function nextBarIndex(key: string, current: number | null, count: number): number | null {
  if (count <= 0) return null;
  const from = current ?? count;
  switch (key) {
    case 'ArrowLeft':
      return Math.max(0, from - 1);
    case 'ArrowRight':
      return current == null ? count - 1 : Math.min(count - 1, from + 1);
    case 'Home':
      return 0;
    case 'End':
      return count - 1;
    default:
      return null;
  }
}
