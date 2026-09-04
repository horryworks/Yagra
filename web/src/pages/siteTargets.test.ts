// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';

import { expandCidr } from '../lib/cidr';
import type { NodeGroup } from '../types/api';
import {
  defaultChecked,
  hostCount,
  prefixRows,
  siteTargetOptions,
  specFor,
  sumHosts,
  SWEEP_LIMIT,
  UNSWEEPABLE_REASONS,
} from './siteTargets';

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

const px = (...list: string[]) => list.map((prefix) => ({ prefix, description: '' }));

describe('prefixRows', () => {
  it("describes the lab's Matsuyama prefixes, all sweepable", () => {
    const rows = prefixRows(
      px('192.168.1.0/24', '192.168.2.0/24', '192.168.255.0/24').map((p, i) => ({
        ...p,
        description: ['Matsuyama LAN', 'Matsuyama NAS', 'Matsuyama VPN pool'][i],
      })),
    );
    expect(rows.map((r) => r.prefix)).toEqual([
      '192.168.1.0/24',
      '192.168.2.0/24',
      '192.168.255.0/24',
    ]);
    expect(rows.map((r) => r.hosts)).toEqual([254, 254, 254]);
    expect(rows.map((r) => r.description)).toEqual([
      'Matsuyama LAN',
      'Matsuyama NAS',
      'Matsuyama VPN pool',
    ]);
    expect(rows.every((r) => r.unsweepable === undefined)).toBe(true);
  });

  // 🚨 The display number and the expander must not drift: the row promises "254 addresses" and
  // the sweep must then send 254. Checked against the expander itself, not against a repeat of the
  // arithmetic, over the shapes where the two rules differ (/31 and /32 keep every address).
  it('counts exactly what the expander would produce, for every shape it accepts', () => {
    for (const p of ['192.168.1.0/24', '10.0.0.0/30', '10.0.0.0/31', '10.0.0.5/32', '10.0.0.0/22']) {
      const [row] = prefixRows(px(p));
      expect(row.unsweepable, p).toBeUndefined();
      expect(row.hosts, p).toBe(expandCidr(p).length);
    }
  });

  it('marks an IPv6 prefix as never sweepable, and still names it', () => {
    const [row] = prefixRows([{ prefix: '2001:db8::/64', description: 'Matsuyama v6' }]);
    expect(row.unsweepable).toBe('v6');
    expect(row.description).toBe('Matsuyama v6');
  });

  // A different problem with a different answer: this one the operator could narrow in NetBox.
  it('marks a prefix that is merely too large, with the count that says why', () => {
    const [row] = prefixRows(px('10.0.0.0/8'));
    expect(row.unsweepable).toBe('tooLarge');
    expect(row.hosts).toBe(2 ** 24 - 2);
  });

  it('treats a /21 as too large, because one sweep is capped at 1024 addresses', () => {
    expect(prefixRows(px('10.0.0.0/21'))[0].unsweepable).toBe('tooLarge');
    expect(prefixRows(px('10.0.0.0/22'))[0].unsweepable).toBeUndefined();
  });

  it('has a reason token for every way a row can be unsweepable', () => {
    const seen = new Set(
      prefixRows(px('2001:db8::/64', '10.0.0.0/8'))
        .map((r) => r.unsweepable)
        .filter(Boolean),
    );
    expect([...seen].sort()).toEqual([...UNSWEEPABLE_REASONS].sort());
  });
});

describe('defaultChecked', () => {
  it('ticks every sweepable prefix and nothing else', () => {
    const rows = prefixRows(px('192.168.1.0/24', '2001:db8::/64', '10.0.0.0/8', '192.168.2.0/24'));
    expect([...defaultChecked(rows)].sort()).toEqual(['192.168.1.0/24', '192.168.2.0/24']);
  });

  it('ticks nothing when no prefix can be swept', () => {
    expect(defaultChecked(prefixRows(px('2001:db8::/64')))).toEqual(new Set());
  });
});

describe('specFor', () => {
  const rows = prefixRows(px('192.168.1.0/24', '192.168.2.0/24', '192.168.255.0/24'));

  it('joins the ticked prefixes in the order they are drawn', () => {
    expect(specFor(rows, new Set(['192.168.255.0/24', '192.168.1.0/24']))).toBe(
      '192.168.1.0/24, 192.168.255.0/24',
    );
  });

  it('is empty when nothing is ticked', () => {
    expect(specFor(rows, new Set())).toBe('');
  });

  // A prefix that left NetBox between choosing the site and pressing Scan is still in the set but
  // no longer in the rows. Building from the rows is what stops it reaching the sweep.
  it('ignores a ticked prefix the folder no longer has', () => {
    expect(specFor(rows, new Set(['192.168.1.0/24', '10.9.9.0/24']))).toBe('192.168.1.0/24');
  });

  it('never emits a prefix that cannot be swept, even if it is ticked', () => {
    const mixed = prefixRows(px('192.168.1.0/24', '2001:db8::/64'));
    expect(specFor(mixed, new Set(['192.168.1.0/24', '2001:db8::/64']))).toBe('192.168.1.0/24');
  });
});

describe('sumHosts', () => {
  const rows = prefixRows(px('192.168.1.0/24', '192.168.2.0/24', '192.168.255.0/24'));

  it("adds up the lab's three Matsuyama /24s", () => {
    expect(sumHosts(rows, defaultChecked(rows))).toBe(762);
  });

  it('drops to one range when two are unticked', () => {
    expect(sumHosts(rows, new Set(['192.168.2.0/24']))).toBe(254);
  });

  it('is zero with nothing ticked', () => {
    expect(sumHosts(rows, new Set())).toBe(0);
  });

  // The case the sum exists for: past the limit `hostCount` answers null, and a total that simply
  // vanished would make an over-large selection look like an empty one.
  it('still answers past the sweep limit, where the exact count cannot', () => {
    const wide = prefixRows(px('10.0.0.0/22', '10.1.0.0/22'));
    const all = defaultChecked(wide);
    expect(sumHosts(wide, all)).toBe(2044);
    expect(sumHosts(wide, all)).toBeGreaterThan(SWEEP_LIMIT);
    expect(hostCount(specFor(wide, all))).toBeNull();
  });
});

describe('hostCount', () => {
  it("counts the lab's three Matsuyama /24s", () => {
    expect(hostCount('192.168.1.0/24, 192.168.2.0/24, 192.168.255.0/24')).toBe(762);
  });

  it('counts all four lab prefixes, just inside the limit', () => {
    expect(hostCount('192.168.0.0/24, 192.168.1.0/24, 192.168.2.0/24, 192.168.255.0/24')).toBe(
      1016,
    );
  });

  // De-duplicates, which is why the picker prefers it over `sumHosts` whenever it answers.
  it('counts an address covered by two ticked prefixes once', () => {
    expect(hostCount('10.0.0.0/24, 10.0.0.0/24')).toBe(254);
  });

  it('is null past the sweep limit', () => {
    expect(hostCount('10.0.0.0/22, 10.0.4.0/24')).toBeNull();
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
