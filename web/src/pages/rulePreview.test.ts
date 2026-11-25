// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import type { CoverageSegment, RulePreview, RulePreviewRow } from '../types/api';
import {
  dayPercent,
  deciderColour,
  deciderOf,
  legendOf,
  nowOnDay,
  nowStretch,
  otherDeciders,
  previewQuery,
  rowZone,
  weekMinuteText,
  type PreviewDraft,
} from './rulePreview';

const NIL = '00000000-0000-0000-0000-000000000000';
const BELOW = 'b0000000-0000-0000-0000-000000000000';
const JSM = 'c0000000-0000-0000-0000-000000000000';
const DAY = 1440;

const draft = (over: Partial<PreviewDraft> = {}): PreviewDraft => ({
  severity: 'critical',
  target: { group_ids: [], tags: ['8x5x5'], join: 'or' },
  when: { mode: 'during', days: [1, 2, 3, 4, 5], start: '08:30', end: '17:20' },
  whenClosed: 'next_rule',
  hold: false,
  channelIds: [JSM],
  ...over,
});

const seg = (
  start: number,
  end: number,
  kind: CoverageSegment['kind'],
  rule_id: string | null,
): CoverageSegment => ({ start, end, kind, rule_id, channel_ids: kind === 'rules' ? [JSM] : [] });

/** The PoC's week: the rule below until 08:30, this rule until 17:20, the rule below again. */
const passedOn: RulePreviewRow = {
  node_count: 312,
  zone: 'Asia/Tokyo',
  rule_ids: [NIL, BELOW],
  segments: [
    seg(0, 8 * 60 + 30, 'rules', BELOW),
    seg(8 * 60 + 30, 17 * 60 + 20, 'rules', NIL),
    seg(17 * 60 + 20, DAY + 8 * 60 + 30, 'rules', BELOW),
  ],
  now_minute: 2 * DAY + 17 * 60 + 41,
  next_change_minute: 3 * DAY + 8 * 60 + 30,
};

/** Weekday keys answer their own short name with a capital, as the English locale does. */
const t = ((k: string) => {
  const day = k.split('.').pop() ?? k;
  return day.charAt(0).toUpperCase() + day.slice(1);
}) as unknown as TFunction;

describe('the rule dialog\'s week', () => {
  it('asks about the draft as the dialog holds it', () => {
    expect(previewQuery(draft({ ruleId: 'r1' }))).toEqual({
      rule_id: 'r1',
      severity: 'critical',
      tags: '8x5x5',
      mode: 'during',
      days: '1,2,3,4,5',
      start_minute: 510,
      end_minute: 1040,
      when_closed: 'next_rule',
      channel_ids: JSM,
    });
  });

  it('newline-joins tags, which may hold a comma, and sends hours only for a timed rule', () => {
    const q = previewQuery(
      draft({
        severity: '',
        target: { group_ids: ['g1', 'g2'], tags: ['a,b', 'c'], join: 'and' },
        when: { mode: 'always', days: [], start: '', end: '' },
        whenClosed: 'hold',
      }),
    );
    expect(q).toEqual({ tags: 'a,b\nc', group_ids: 'g1,g2', join: 'and', mode: 'always', channel_ids: JSM });
  });

  it('sends no channels for a holding rule, and asks nothing while the hours cannot be read', () => {
    expect(previewQuery(draft({ hold: true }))).toMatchObject({ hold: true });
    expect(previewQuery(draft({ hold: true }))).not.toHaveProperty('channel_ids');
    expect(previewQuery(draft({ when: { mode: 'during', days: [], start: '08:30', end: '17:20' } }))).toBeNull();
    expect(previewQuery(draft({ when: { mode: 'during', days: [1], start: '8', end: '17:20' } }))).toBeNull();
  });

  it('tells this rule from another rule that sends to the same channel', () => {
    expect(deciderOf(passedOn.segments[1], NIL)).toEqual({ kind: 'this' });
    expect(deciderOf(passedOn.segments[2], NIL)).toEqual({ kind: 'rule', ruleId: BELOW });
    expect(deciderOf(seg(0, 1, 'held', NIL), NIL)).toEqual({ kind: 'held' });
    expect(deciderOf(seg(0, 1, 'catch_all', null), NIL)).toEqual({ kind: 'catch_all' });
    expect(deciderOf(seg(0, 1, 'nowhere', null), NIL)).toEqual({ kind: 'nowhere' });
  });

  it('gives this rule and the rule below different colours, and draws held as a stripe', () => {
    const others = otherDeciders(passedOn, NIL);
    expect(others).toEqual([BELOW]);
    const mine = deciderColour({ kind: 'this' }, others);
    const below = deciderColour({ kind: 'rule', ruleId: BELOW }, others);
    expect(mine).not.toBe(below);
    expect(deciderColour({ kind: 'held' }, others)).toBeNull();
    expect(deciderColour({ kind: 'nowhere' }, others)).toBe('var(--status-critical)');
  });

  it('lists this rule first, the other rules in order, then held, each once', () => {
    const held: RulePreviewRow = {
      ...passedOn,
      segments: [...passedOn.segments, seg(DAY + 8 * 60 + 30, DAY + 9 * 60, 'held', NIL)],
    };
    expect(legendOf(held, NIL).map((e) => e.decider.kind)).toEqual(['this', 'rule', 'held']);
    expect(legendOf(held, NIL)[0].channels).toEqual([JSM]);
  });

  it('finds the stretch now falls in and where to mark it', () => {
    const wednesday: RulePreviewRow = {
      ...passedOn,
      segments: [seg(0, 7 * DAY, 'rules', BELOW)],
    };
    expect(nowStretch(wednesday)?.rule_id).toBe(BELOW);
    expect(nowOnDay(wednesday)).toEqual({ day: 2, left: dayPercent(17 * 60 + 41) });
    expect(nowStretch({ ...wednesday, segments: [] })).toBeNull();
  });

  it('writes a minute of the week as the weekday and the time', () => {
    expect(weekMinuteText(t, 3 * DAY + 8 * 60 + 30)).toBe('Thu 08:30');
    expect(weekMinuteText(t, 0)).toBe('Mon 00:00');
  });

  it('names a row\'s zone, or the deployment\'s default for a row with none', () => {
    const preview = { default_zone: 'UTC' } as RulePreview;
    expect(rowZone(preview, passedOn)).toEqual({ zone: 'Asia/Tokyo', isDefault: false });
    expect(rowZone(preview, { ...passedOn, zone: null })).toEqual({ zone: 'UTC', isDefault: true });
  });
});
