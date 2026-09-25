// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { gapReason, moreDevices, summarizeGaps, type PrefixGap, type PrefixGapReport } from './prefixGaps';

const gap = (over: Partial<PrefixGap>): PrefixGap => ({
  subnet: '10.1.2.0/24',
  kind: 'unregistered',
  range: null,
  range_group: null,
  range_group_name: null,
  node_count: 1,
  seen_on: [{ node_id: 'n1', ifindex: 1, if_name: 'Vlan10', ip: '10.1.2.1' }],
  ...over,
});

const report = (over: Partial<PrefixGapReport>): PrefixGapReport => ({
  group_id: 'g',
  nodes_total: 3,
  nodes_with_addresses: 2,
  nodes_truncated: 0,
  subnets_checked: 4,
  gaps: [],
  ...over,
});

describe('summarizeGaps', () => {
  it('makes no claim when no device reported its addresses', () => {
    expect(summarizeGaps(report({ nodes_with_addresses: 0 }))).toEqual({ kind: 'noData', total: 3 });
  });

  it('says how much was read when nothing is missing', () => {
    expect(summarizeGaps(report({}))).toEqual({ kind: 'clean', read: 2, total: 3 });
  });

  it('counts the gaps', () => {
    expect(summarizeGaps(report({ gaps: [gap({}), gap({})] }))).toEqual({
      kind: 'gaps',
      count: 2,
      read: 2,
      total: 3,
    });
  });
});

describe('gapReason', () => {
  it('names the folder and range when the caller may see them', () => {
    expect(
      gapReason(gap({ kind: 'other_folder', range: '10.1.0.0/16', range_group_name: 'site-b' })),
    ).toEqual({ key: 'prefixGaps.reason.otherFolder', values: { name: 'site-b', range: '10.1.0.0/16' } });
    expect(
      gapReason(gap({ kind: 'parent_only', range: '10.0.0.0/8', range_group_name: 'region' })),
    ).toEqual({ key: 'prefixGaps.reason.parentOnly', values: { name: 'region', range: '10.0.0.0/8' } });
    expect(gapReason(gap({ kind: 'partial', range: '10.1.2.0/25' }))).toEqual({
      key: 'prefixGaps.reason.partial',
      values: { range: '10.1.2.0/25' },
    });
  });

  it('falls back to the bare kind, never to unregistered, when the range is withheld', () => {
    expect(gapReason(gap({ kind: 'other_folder' })).key).toBe('prefixGaps.kind.other_folder');
    expect(gapReason(gap({ kind: 'parent_only' })).key).toBe('prefixGaps.kind.parent_only');
  });

  it('says unregistered only for unregistered', () => {
    expect(gapReason(gap({})).key).toBe('prefixGaps.kind.unregistered');
  });
});

describe('moreDevices', () => {
  it('counts devices beyond the listed places', () => {
    expect(moreDevices(gap({ node_count: 9 }))).toBe(8);
    expect(moreDevices(gap({ node_count: 1 }))).toBe(0);
  });
});
