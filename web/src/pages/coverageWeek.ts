// SPDX-License-Identifier: AGPL-3.0-only
// Coverage (ADR-262 decision 8): the judgement behind the week strip under the routing rules. The
// server decides where an alert goes in every stretch of the week; this only cuts the stretches
// into days, names a row and counts the nodes that have a stretch where nobody hears an alert.

import type { CoverageRow, CoverageSegment } from '../types/api';

/** Minutes in a day; a stretch is counted in minutes from Monday 00:00 local time. */
const DAY = 1440;

/** The stretches of one day (0 = Monday), clipped to it, start and end in minutes of that day. */
export function daySlices(segments: readonly CoverageSegment[], day: number): CoverageSegment[] {
  const from = day * DAY;
  const to = from + DAY;
  return segments
    .filter((s) => s.end > from && s.start < to)
    .map((s) => ({
      ...s,
      start: Math.max(s.start, from) - from,
      end: Math.min(s.end, to) - from,
    }));
}

/** A row's name: the rules that reach its nodes, or `null` when none does. A rule the list no
 *  longer holds is left out rather than shown by its id. */
export function rowRuleNames(row: CoverageRow, ruleName: (id: string) => string | undefined): string | null {
  const names = row.rule_ids.map(ruleName).filter((n): n is string => !!n);
  return names.length > 0 ? names.join(' + ') : null;
}

/** How many nodes have a stretch of the week where an alert is held, and where it reaches no
 *  channel - what the strip exists to make visible. */
export function coverageGaps(rows: readonly CoverageRow[]): { held: number; nowhere: number } {
  let held = 0;
  let nowhere = 0;
  for (const r of rows) {
    if (r.segments.some((s) => s.kind === 'held')) held += r.node_count;
    if (r.segments.some((s) => s.kind === 'nowhere')) nowhere += r.node_count;
  }
  return { held, nowhere };
}

/** How many nodes the week covers: every row, once each. */
export function coverageNodes(rows: readonly CoverageRow[]): number {
  return rows.reduce((n, r) => n + r.node_count, 0);
}

/** A channel's colour on the strip: the categorical series, by the order channels are first met. */
export function channelColour(index: number): string {
  return `var(--series-${(index % 8) + 1})`;
}

/** Every channel a strip paints, in first-seen order, so each keeps one colour across rows. */
export function paintedChannels(rows: readonly CoverageRow[]): string[] {
  const seen: string[] = [];
  for (const r of rows) {
    for (const s of r.segments) {
      for (const c of s.channel_ids) if (!seen.includes(c)) seen.push(c);
    }
  }
  return seen;
}
