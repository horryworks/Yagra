// SPDX-License-Identifier: AGPL-3.0-only
// The rule dialog's week (ADR-266 decision 5): where the alerts of the nodes a draft rule reaches
// would go, stretch by stretch, and which rule decides each. The server judges every minute; this
// builds the question from the dialog, names who decides a stretch, picks its colour and writes
// the "now" line. Kept out of the `.tsx` so Vitest can reach it.

import type { TFunction } from 'i18next';
import { PALETTE } from '../components/MetricChart/palette';
import type {
  CoverageSegment,
  Join,
  RulePreview,
  RulePreviewRow,
  Severity,
  WhenClosed,
  WhenMode,
} from '../types/api';
import { minutesToTime, weekdayName } from './deliveryHours';
import { whenBodyFrom, type WhenDraft } from './ruleWhen';

const DAY = 1440;

/** The dialog's fields, as the week asks about them. */
export interface PreviewDraft {
  /** The rule being edited; absent for a new rule. */
  ruleId?: string;
  severity: '' | Severity;
  /** The folders and tags it names, the way the rule body sends them (`appliesToBody`). */
  target: { group_ids: readonly string[]; tags: readonly string[]; join: Join };
  when: WhenDraft;
  whenClosed: WhenClosed;
  hold: boolean;
  channelIds: readonly string[];
}

/** What `GET /api/v1/routing-rules/preview` takes. */
export interface PreviewQuery {
  rule_id?: string;
  severity?: Severity;
  tags?: string;
  group_ids?: string;
  join?: Join;
  mode: WhenMode;
  days?: string;
  start_minute?: number;
  end_minute?: number;
  when_closed?: WhenClosed;
  hold?: boolean;
  channel_ids?: string;
}

/** The question for the draft, or `null` while its hours cannot be read - the dialog says what is
 *  wrong with them, and the week keeps the last answer meanwhile. Tags are newline-joined (a tag may
 *  hold a comma, never a line break); ids are comma-joined. */
export function previewQuery(d: PreviewDraft): PreviewQuery | null {
  const body = whenBodyFrom(d.when);
  if ('error' in body) return null;
  const w = body.when;
  const q: PreviewQuery = { mode: w.mode ?? 'always' };
  if (d.ruleId) q.rule_id = d.ruleId;
  if (d.severity) q.severity = d.severity;
  if (d.target.tags.length > 0) q.tags = d.target.tags.join('\n');
  if (d.target.group_ids.length > 0) q.group_ids = d.target.group_ids.join(',');
  if (d.target.join === 'and') q.join = 'and';
  if (q.mode !== 'always') {
    q.days = (w.days ?? []).join(',');
    q.start_minute = w.start_minute ?? 0;
    q.end_minute = w.end_minute ?? 0;
    q.when_closed = d.whenClosed;
  }
  if (d.hold) q.hold = true;
  else if (d.channelIds.length > 0) q.channel_ids = d.channelIds.join(',');
  return q;
}

/** Who decides a stretch: the rule being edited, another rule, the catch-all, nobody yet (held) or
 *  nobody at all. A stretch held by a rule is drawn as held, whichever rule holds it. */
export type Decider =
  | { kind: 'this' }
  | { kind: 'rule'; ruleId: string }
  | { kind: 'catch_all' }
  | { kind: 'held' }
  | { kind: 'nowhere' };

export function deciderOf(seg: CoverageSegment, thisRule: string): Decider {
  switch (seg.kind) {
    case 'held':
      return { kind: 'held' };
    case 'nowhere':
      return { kind: 'nowhere' };
    case 'catch_all':
      return { kind: 'catch_all' };
    case 'rules':
      return seg.rule_id === thisRule ? { kind: 'this' } : { kind: 'rule', ruleId: seg.rule_id ?? '' };
  }
}

/** One key per decider, for grouping stretches and legend entries. */
export function deciderKey(d: Decider): string {
  return d.kind === 'rule' ? `rule:${d.ruleId}` : d.kind;
}

/** A decider's fill: the first series colour for this rule, the next ones for the other rules in
 *  the order they decide, a neutral for the catch-all and the critical colour for nobody. `held` is
 *  a stripe the stylesheet draws, so it has no colour here. */
export function deciderColour(d: Decider, others: readonly string[]): string | null {
  switch (d.kind) {
    case 'this':
      return PALETTE[0];
    case 'rule': {
      const i = Math.max(0, others.indexOf(d.ruleId));
      return PALETTE[1 + (i % (PALETTE.length - 1))];
    }
    case 'catch_all':
      return 'var(--text-tertiary)';
    case 'held':
      return null;
    case 'nowhere':
      return 'var(--status-critical)';
  }
}

/** The other rules that decide somewhere in a row's week, in the order they stand. */
export function otherDeciders(row: RulePreviewRow, thisRule: string): string[] {
  const seen = new Set(
    row.segments.map((s) => deciderOf(s, thisRule)).filter((d) => d.kind === 'rule').map(deciderKey),
  );
  return row.rule_ids.filter((id) => id !== thisRule && seen.has(`rule:${id}`));
}

/** What the legend lists: each decider met in the week once, this rule first, then the other
 *  rules in the order they stand, then the catch-all, held and nobody. `channels` are the channel
 *  ids of the first stretch it decides. */
export interface LegendEntry {
  decider: Decider;
  channels: string[];
}

export function legendOf(row: RulePreviewRow, thisRule: string): LegendEntry[] {
  const first = new Map<string, LegendEntry>();
  for (const seg of row.segments) {
    const decider = deciderOf(seg, thisRule);
    const key = deciderKey(decider);
    if (!first.has(key)) first.set(key, { decider, channels: [...seg.channel_ids] });
  }
  const order = [
    'this',
    ...otherDeciders(row, thisRule).map((id) => `rule:${id}`),
    'catch_all',
    'held',
    'nowhere',
  ];
  return order.map((k) => first.get(k)).filter((e): e is LegendEntry => e !== undefined);
}

/** A minute of the week as "Wed 17:41", read with the screen's weekday names. */
export function weekMinuteText(t: TFunction, minute: number): string {
  const m = ((minute % (7 * DAY)) + 7 * DAY) % (7 * DAY);
  return `${weekdayName(t, Math.floor(m / DAY) + 1)} ${minutesToTime(m % DAY)}`;
}

/** The stretch a row's "now" falls in, or `null` when the week does not cover it. */
export function nowStretch(row: RulePreviewRow): CoverageSegment | null {
  return row.segments.find((s) => s.start <= row.now_minute && row.now_minute < s.end) ?? null;
}

/** A marker's place along a day bar, as a percentage of the day. */
export function dayPercent(minuteOfDay: number): string {
  return `${((minuteOfDay / DAY) * 100).toFixed(3)}%`;
}

/** The day (0 = Monday) a row's "now" falls on, and where along it. */
export function nowOnDay(row: RulePreviewRow): { day: number; left: string } {
  return { day: Math.floor(row.now_minute / DAY), left: dayPercent(row.now_minute % DAY) };
}

/** The zone a row keeps time in, named: its own, or the deployment's default. */
export function rowZone(preview: RulePreview, row: RulePreviewRow): { zone: string; isDefault: boolean } {
  return row.zone
    ? { zone: row.zone, isDefault: false }
    : { zone: preview.default_zone, isDefault: true };
}
