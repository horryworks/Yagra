// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { SubnetOverlap, SubnetOverlapsView } from '../types/api';
import {
  draftFrom,
  emptyKey,
  openKindCounts,
  overlapsOn,
  rangeBars,
  ruleBody,
  suggestedRule,
  toggled,
  visibleSites,
} from './subnetOverlaps';

function overlap(p: Partial<SubnetOverlap>): SubnetOverlap {
  return {
    key: 'same:192.0.2.0/24',
    kind: 'same_address',
    status: 'open',
    subnet: '192.0.2.0/24',
    inner: [],
    inner_count: 0,
    shared_addresses: [],
    site_count: 2,
    hidden_sites: 0,
    node_count: 2,
    places: [],
    place_count: 0,
    hint: null,
    excluded_by: [],
    note: null,
    ...p,
  };
}

function view(overlaps: SubnetOverlap[], p: Partial<SubnetOverlapsView> = {}): SubnetOverlapsView {
  return {
    overlaps,
    counts: { open: 0, intentional: 0, excluded: 0 },
    rules: [],
    nodes_total: 10,
    nodes_with_addresses: 10,
    nodes_truncated: 0,
    subnets_checked: 5,
    ...p,
  };
}

const place = (site: string | null, name: string | null, node = 'n1') => ({
  site_id: site,
  site_name: name,
  node_id: node,
  node_name: 'rt-01',
  ifindex: 1,
  address: '192.0.2.1/24',
  subnet: '192.0.2.0/24',
});

describe('subnet overlaps', () => {
  it('puts each overlap on its own tab and narrows by kind', () => {
    const v = view([
      overlap({ key: 'a', status: 'open', kind: 'same_address' }),
      overlap({ key: 'b', status: 'open', kind: 'nested' }),
      overlap({ key: 'c', status: 'excluded', kind: 'same_range' }),
      overlap({ key: 'd', status: 'intentional' }),
    ]);
    expect(overlapsOn(v, 'open', null).map((o) => o.key)).toEqual(['a', 'b']);
    expect(overlapsOn(v, 'open', 'nested').map((o) => o.key)).toEqual(['b']);
    expect(overlapsOn(v, 'excluded', null).map((o) => o.key)).toEqual(['c']);
    expect(overlapsOn(null, 'open', null)).toEqual([]);
    expect(openKindCounts(v)).toEqual({ same_address: 1, nested: 1, same_range: 0 });
  });

  it('lists each visible site once, keeping a root place as a nameless site', () => {
    const o = overlap({
      places: [place('s1', 'tokyo'), place('s1', 'tokyo', 'n2'), place(null, null, 'n3')],
    });
    expect(visibleSites(o)).toEqual([
      { key: 's1', name: 'tokyo' },
      { key: '', name: null },
    ]);
  });

  it('suggests a rule only for a hint that names a word', () => {
    expect(suggestedRule(overlap({ hint: { kind: 'wan', word: 'onu' } }))).toEqual({
      port_text: 'onu',
      reason: 'wan',
      note: '',
      enabled: true,
    });
    expect(suggestedRule(overlap({ hint: { kind: 'redundancy', word: 'ha' } }))?.reason).toBe(
      'redundancy',
    );
    expect(suggestedRule(overlap({ hint: { kind: 'shared_line' } }))).toBeNull();
    expect(suggestedRule(overlap({ hint: { kind: 'template' } }))).toBeNull();
    expect(suggestedRule(overlap({}))).toBeNull();
  });

  it('refuses a rule naming nothing or a range with no length, and trims what it sends', () => {
    expect(ruleBody(draftFrom(null))).toEqual({ refuse: 'rules.form.needSomething' });
    expect(ruleBody({ ...draftFrom(null), range: '10.0.0.0' })).toEqual({
      refuse: 'rules.form.badRange',
    });
    expect(ruleBody({ ...draftFrom(null), range: ' 2001:db8::/32 ' })).toEqual({
      body: { range: '2001:db8::/32', port_text: null, reason: 'wan', note: '', enabled: true },
    });
    const fromHint = draftFrom({ port_text: 'wwan', reason: 'wan' });
    expect(ruleBody({ ...fromHint, note: ' LTE ' })).toEqual({
      body: { range: null, port_text: 'wwan', reason: 'wan', note: 'LTE', enabled: true },
    });
  });

  it('flips a rule by sending the whole rule back with the switch reversed', () => {
    expect(
      toggled({ range: '100.64.0.0/10', port_text: null, reason: 'wan', note: 'cgnat', enabled: true }),
    ).toEqual({ range: '100.64.0.0/10', port_text: null, reason: 'wan', note: 'cgnat', enabled: false });
  });

  it('places each inner range where it sits inside the outer one', () => {
    const bars = rangeBars(
      overlap({ kind: 'nested', subnet: '10.30.0.0/16', inner: ['10.30.128.0/24', '10.30.0.0/17'] }),
    );
    expect(bars[0]).toEqual({ subnet: '10.30.0.0/16', leftPct: 0, widthPct: 100, outer: true });
    expect(bars[1].leftPct).toBe(50);
    expect(bars[1].widthPct).toBeCloseTo(100 / 256);
    expect(bars[2]).toEqual({ subnet: '10.30.0.0/17', leftPct: 0, widthPct: 50, outer: false });
    expect(rangeBars(overlap({ kind: 'same_address' }))).toEqual([]);
    expect(rangeBars(overlap({ kind: 'nested', subnet: '2001:db8::/48' }))).toEqual([]);
  });

  it('only says nothing overlaps when every visible device has reported its addresses', () => {
    expect(emptyKey(view([]), 'open')).toEqual({ key: 'empty.open', missing: 0 });
    expect(emptyKey(view([], { nodes_with_addresses: 7 }), 'open')).toEqual({
      key: 'empty.openPartial',
      missing: 3,
    });
    expect(emptyKey(view([], { nodes_with_addresses: 0 }), 'open').key).toBe('empty.noAddresses');
    expect(emptyKey(view([]), 'excluded').key).toBe('empty.excluded');
  });
});
