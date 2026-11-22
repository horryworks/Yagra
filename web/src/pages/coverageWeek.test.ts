// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { PALETTE } from '../components/MetricChart/palette';
import type { CoverageRow } from '../types/api';
import {
  channelColour,
  coverageGaps,
  coverageNodes,
  daySlices,
  paintedChannels,
  rowRuleNames,
} from './coverageWeek';

const row = (over: Partial<CoverageRow>): CoverageRow => ({
  rule_ids: [],
  zone: null,
  node_count: 1,
  node_ids: [],
  segments: [],
  ...over,
});

describe('coverage strip', () => {
  it('cuts a stretch that spans midnight into the two days it covers', () => {
    const segs = [
      { start: 0, end: 510, kind: 'held' as const, channel_ids: [] },
      { start: 510, end: 1040, kind: 'rules' as const, channel_ids: ['mail'] },
      { start: 1040, end: 1440 + 510, kind: 'held' as const, channel_ids: [] },
    ];
    expect(daySlices(segs, 0).map((s) => [s.start, s.end])).toEqual([
      [0, 510],
      [510, 1040],
      [1040, 1440],
    ]);
    expect(daySlices(segs, 1).map((s) => [s.start, s.end, s.kind])).toEqual([[0, 510, 'held']]);
  });

  it('names a row by its rules, and none when no rule reaches it', () => {
    const names: Record<string, string> = { a: 'Day desk', b: 'Night page' };
    expect(rowRuleNames(row({ rule_ids: ['a', 'b'] }), (id) => names[id])).toBe(
      'Day desk + Night page',
    );
    expect(rowRuleNames(row({ rule_ids: [] }), (id) => names[id])).toBeNull();
  });

  it('counts the nodes that have a held stretch and those that reach nobody', () => {
    const rows = [
      row({ node_count: 3, segments: [{ start: 0, end: 10, kind: 'held', channel_ids: [] }] }),
      row({ node_count: 2, segments: [{ start: 0, end: 10, kind: 'nowhere', channel_ids: [] }] }),
      row({ node_count: 9, segments: [{ start: 0, end: 10, kind: 'rules', channel_ids: ['m'] }] }),
    ];
    expect(coverageGaps(rows)).toEqual({ held: 3, nowhere: 2 });
    expect(coverageNodes(rows)).toBe(rows.reduce((n, r) => n + r.node_count, 0));
    expect(paintedChannels(rows)).toEqual(['m']);
    expect(channelColour(PALETTE.length)).toBe(channelColour(0));
    for (let i = 0; i < PALETTE.length * 2; i++) expect(PALETTE).toContain(channelColour(i));
  });
});
