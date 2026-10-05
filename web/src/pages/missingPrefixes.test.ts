// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { PrefixGap, PrefixGapSitesView, SitePrefixGaps } from '../types/api';
import {
  gapsCsv,
  kindCounts,
  allGaps,
  siteKey,
  sitesOn,
  statusCounts,
  subnetRows,
} from './missingPrefixes';

const gap = (subnet: string, kind: PrefixGap['kind'], node = 'n1', ip = '10.0.0.1'): PrefixGap => ({
  subnet,
  kind,
  range: kind === 'unregistered' ? null : '10.0.0.0/8',
  range_group: null,
  range_group_name: null,
  node_count: 1,
  seen_on: [{ node_id: node, ifindex: 1, if_name: 'Vlan1', ip }],
});

const site = (
  id: string | null,
  name: string | null,
  status: SitePrefixGaps['status'],
  gaps: PrefixGap[] = [],
): SitePrefixGaps => ({
  site_id: id,
  name,
  path: ['east'],
  is_site: id !== null,
  status,
  nodes_total: 2,
  nodes_with_addresses: status === 'no_data' ? 0 : 2,
  nodes_truncated: 0,
  prefixes: 1,
  subnets_checked: gaps.length + 1,
  gap_count: gaps.length,
  gaps,
});

const view = (): PrefixGapSitesView => ({
  sites: [
    site('a', 'hq', 'gaps', [gap('198.51.100.0/29', 'unregistered', 'rt'), gap('10.1.30.0/24', 'parent_only')]),
    site('b', 'branch-a', 'gaps', [gap('192.168.10.0/24', 'other_folder')]),
    site('c', 'branch-b', 'clean'),
    site(null, null, 'no_data'),
  ],
  nodes_total: 8,
  nodes_with_addresses: 6,
  nodes_truncated: 0,
  subnets_checked: 6,
  gaps_total: 3,
  gaps_listed: 3,
});

const names: Record<string, string> = { rt: 'rt-01', n1: 'sw-01' };
const nodeName = (id: string) => names[id] ?? id;
const none = { kind: null, q: '' };

describe('missing IP prefixes', () => {
  it('puts each site on the tab its status names', () => {
    expect(sitesOn(view(), 'gaps', none, nodeName).map((r) => r.site.name)).toEqual(['hq', 'branch-a']);
    expect(sitesOn(view(), 'clean', none, nodeName).map((r) => r.site.name)).toEqual(['branch-b']);
    expect(sitesOn(view(), 'no_data', none, nodeName).map((r) => siteKey(r.site))).toEqual(['root']);
    expect(statusCounts(view())).toEqual({ gaps: 2, clean: 1, no_data: 1 });
  });

  it('a kind tile keeps only that kind, and a site left with nothing drops off the gaps tab', () => {
    const rows = sitesOn(view(), 'gaps', { kind: 'other_folder', q: '' }, nodeName);
    expect(rows.map((r) => r.site.name)).toEqual(['branch-a']);
    expect(rows[0].gaps.map((g) => g.subnet)).toEqual(['192.168.10.0/24']);
  });

  it('the search matches a subnet, an address, a device name or the site', () => {
    const q = (term: string) => subnetRows(view(), { kind: null, q: term }, nodeName).map((r) => r.gap.subnet);
    expect(q('198.51')).toEqual(['198.51.100.0/29']);
    expect(q('RT-01')).toEqual(['198.51.100.0/29']);
    expect(q('branch-a')).toEqual(['192.168.10.0/24']);
    expect(q('east')).toHaveLength(3);
    // The tabs without gaps match the site only.
    expect(sitesOn(view(), 'clean', { kind: null, q: 'nothing' }, nodeName)).toEqual([]);
    expect(sitesOn(view(), 'clean', { kind: null, q: 'b' }, nodeName)).toHaveLength(1);
  });

  it('the subnet list is ordered by kind, then subnet', () => {
    expect(subnetRows(view(), none, nodeName).map((r) => [r.gap.kind, r.gap.subnet])).toEqual([
      ['unregistered', '198.51.100.0/29'],
      ['other_folder', '192.168.10.0/24'],
      ['parent_only', '10.1.30.0/24'],
    ]);
  });

  it('counts each kind over every listed gap', () => {
    expect(kindCounts(allGaps(view()))).toEqual({
      unregistered: 1,
      partial: 0,
      other_folder: 1,
      parent_only: 1,
    });
    expect(kindCounts(allGaps(null))).toEqual({ unregistered: 0, partial: 0, other_folder: 0, parent_only: 0 });
  });

  it('the CSV has one line per gap and neutralizes a value a spreadsheet would run', () => {
    const v = view();
    v.sites[0].name = '=HYPERLINK("x")';
    const csv = gapsCsv(subnetRows(v, none, nodeName), nodeName);
    const lines = csv.split('\r\n');
    expect(lines[0]).toBe('"subnet","kind","site","folders","range","devices","seen_on"');
    expect(lines).toHaveLength(4);
    expect(lines[1]).toContain(`"'=HYPERLINK(""x"")"`);
    expect(lines[1]).toContain('"rt-01 Vlan1 10.0.0.1"');
  });
});
