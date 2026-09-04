// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';

import type { NodeGroup } from '../types/api';
import { hostCount, prefixesToSpec, siteTargetOptions } from './siteTargets';

/** A folder row with only the fields this module reads. */
function group(id: string, name: string, parent: string | null, prefixes: string[]): NodeGroup {
  return {
    id,
    name,
    group_type: 'site',
    parent_id: parent,
    sort_order: 0,
    latitude: null,
    longitude: null,
    effective_latitude: null,
    effective_longitude: null,
    geo_source: 'unset',
    geo_group: null,
    pool: null,
    prefixes: prefixes.map((prefix) => ({ prefix, description: '' })),
  } as NodeGroup;
}

describe('prefixesToSpec', () => {
  it("joins the lab's Matsuyama prefixes into one spec", () => {
    const out = prefixesToSpec([
      { prefix: '192.168.1.0/24' },
      { prefix: '192.168.2.0/24' },
      { prefix: '192.168.255.0/24' },
    ]);
    expect(out.spec).toBe('192.168.1.0/24, 192.168.2.0/24, 192.168.255.0/24');
    expect(out.skippedV6).toBe(0);
  });

  // 🚨 One v6 token makes `expandTargets` reject the entire spec, so dropping them is what keeps a
  // dual-stack site sweepable at all — and the count is what stops that being silent.
  it('drops IPv6 prefixes and says how many', () => {
    const out = prefixesToSpec([
      { prefix: '192.168.1.0/24' },
      { prefix: '2001:db8::/64' },
      { prefix: 'fd00::/48' },
    ]);
    expect(out.spec).toBe('192.168.1.0/24');
    expect(out.skippedV6).toBe(2);
  });

  it('is empty for a folder with no prefixes', () => {
    expect(prefixesToSpec([])).toEqual({ spec: '', skippedV6: 0 });
  });

  it('reports an all-IPv6 folder rather than pretending it produced a spec', () => {
    expect(prefixesToSpec([{ prefix: '2001:db8::/64' }])).toEqual({ spec: '', skippedV6: 1 });
  });
});

describe('hostCount', () => {
  it("counts the lab's three Matsuyama /24s", () => {
    // 3 x (256 - network - broadcast).
    expect(hostCount('192.168.1.0/24, 192.168.2.0/24, 192.168.255.0/24')).toBe(762);
  });

  it('counts all four lab prefixes, just inside the 1024 cap', () => {
    expect(
      hostCount('192.168.0.0/24, 192.168.1.0/24, 192.168.2.0/24, 192.168.255.0/24'),
    ).toBe(1016);
  });

  // The case the count exists for: past the cap `expandTargets` returns nothing, and without a
  // number on screen pressing Start looks like it did nothing at all.
  it('is null past the 1024-address cap', () => {
    expect(hostCount('10.0.0.0/22, 10.0.4.0/24')).toBeNull();
  });

  it('is null for a prefix wider than the expander accepts', () => {
    expect(hostCount('10.0.0.0/8')).toBeNull();
  });

  it('is null for an empty or malformed spec', () => {
    expect(hostCount('')).toBeNull();
    expect(hostCount('   ')).toBeNull();
    expect(hostCount('not-a-prefix')).toBeNull();
  });
});

describe('siteTargetOptions', () => {
  const tree = [
    group('r1', 'Japan', null, []),
    group('r2', 'Ehime', 'r1', []),
    group('s1', 'JPMYJ01 Matsuyama Home', 'r2', ['192.168.1.0/24', '192.168.2.0/24']),
    group('s2', 'JPYOK01 Yokohama Home', 'r1', ['192.168.0.0/24']),
    group('s3', 'Nowhere', 'r1', []),
  ];

  it('offers only folders that carry a prefix', () => {
    expect(siteTargetOptions(tree).map((o) => o.id)).toEqual(['s1', 's2']);
  });

  it('labels a folder by its place in the tree', () => {
    const [first] = siteTargetOptions(tree);
    expect(first.label).toBe('Japan / Ehime / JPMYJ01 Matsuyama Home');
  });

  it('carries the prefixes through in the order they arrived', () => {
    const [first] = siteTargetOptions(tree);
    expect(first.prefixes.map((p) => p.prefix)).toEqual(['192.168.1.0/24', '192.168.2.0/24']);
  });

  // A Region with a prefix of its own is offered; a Region without one is not. That is the whole
  // of "a folder offers its own prefixes and never its descendants'".
  it('offers a Region only when it carries a prefix itself', () => {
    expect(siteTargetOptions(tree).some((o) => o.id === 'r2')).toBe(false);
    const withRegionPrefix = [...tree, group('r3', 'Kanto', 'r1', ['10.0.0.0/24'])];
    expect(siteTargetOptions(withRegionPrefix).some((o) => o.id === 'r3')).toBe(true);
  });

  it('still offers a folder whose prefixes are all IPv6, so the reason can be shown', () => {
    const v6only = [group('s9', 'Osaka', null, ['2001:db8::/64'])];
    expect(siteTargetOptions(v6only).map((o) => o.id)).toEqual(['s9']);
  });

  it('is empty for a deployment with no prefixes anywhere', () => {
    expect(siteTargetOptions([group('a', 'A', null, [])])).toEqual([]);
  });
});
