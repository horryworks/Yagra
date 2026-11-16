// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { InterfaceRow, InterfaceVlan } from '../../types/api';
import { readFilterParams } from '../../lib/columnFilter';
import { applyFilters } from '../../lib/filterPredicate';
import { interfaceColumns } from './tabFilters';
import { gatewayAddresses, interfacesSearch, isUndefinedVlan, vlanPortCounts } from './vlanRows';
import { effectiveVlan } from './interfaceVlan';

const v = (o: Partial<InterfaceVlan>): InterfaceVlan => ({
  mode: 'access',
  native: null,
  access_vlan: null,
  voice_vlan: null,
  allowed: [],
  untagged: [],
  tagged: [],
  lag: null,
  members: [],
  ...o,
});
const row = (ifindex: number, vlan: InterfaceVlan | null): InterfaceRow =>
  ({ ifindex, if_name: `p${ifindex}`, vlan, addresses: [], stale: false }) as unknown as InterfaceRow;

// Two access ports in 20 (one with voice 30), a trunk allowing everything, a hybrid with 30 tagged,
// a Port-channel trunking 20 and its member, a routed port, and a port with nothing reported.
const ROWS: InterfaceRow[] = [
  row(1, v({ access_vlan: 20, voice_vlan: 30 })),
  row(2, v({ access_vlan: 20 })),
  row(3, v({ mode: 'trunk', native: 1, allowed: [{ first: 1, last: 4094 }] })),
  row(4, v({ mode: 'hybrid', native: 1, tagged: [{ first: 30, last: 30 }] })),
  row(8, v({ mode: 'trunk', native: 1, allowed: [{ first: 20, last: 20 }] })),
  row(9, v({ mode: 'member', lag: { ifindex: 8, name: 'Po1' } })),
  row(10, v({ mode: 'not_l2' })),
  row(11, null),
];

describe('vlanPortCounts', () => {
  it('counts access and trunk ports per VLAN', () => {
    const c = vlanPortCounts(ROWS);
    expect(c.get(20)).toEqual({ access: 2, trunk: 2 });
    expect(c.get(30)).toEqual({ access: 1, trunk: 2 });
    // The hybrid's PVID is not a VLAN it carries (its untagged list is empty), as the filter reads it.
    expect(c.get(1)).toEqual({ access: 0, trunk: 2 });
    expect(c.get(4000)).toEqual({ access: 0, trunk: 1 });
  });

  // The promise the tab makes: pressing a count opens exactly that many rows. The Interfaces tab's
  // own filters, read back from the URL the count's link writes, run over the rows the way the tab
  // runs them (with `vlan_effective` attached), through the real `applyFilters`.
  it('opens exactly the rows it counted', () => {
    const counts = vlanPortCounts(ROWS);
    const t = ((k: string) => k) as never;
    const columns = interfaceColumns(t);
    const byIfindex = new Map(ROWS.map((r) => [r.ifindex, r] as const));
    const tabRows = ROWS.map((r) => ({ ...r, vlan_effective: effectiveVlan(r, byIfindex) }));
    let checked = 0;
    for (const vlan of [1, 20, 30, 4000]) {
      for (const group of ['access', 'trunk'] as const) {
        const params = new URLSearchParams(
          interfacesSearch(new URLSearchParams('node=x&interfaces.state=up'), vlan, group).slice(1),
        );
        expect(params.get('tab')).toBe('interfaces');
        expect(params.get('node')).toBe('x');
        expect(params.has('interfaces.state')).toBe(false);
        const shown = applyFilters(tabRows, columns, readFilterParams(columns, params, 'interfaces.'), 0);
        expect(shown.length, `VLAN ${vlan} ${group}`).toBe(counts.get(vlan)?.[group] ?? 0);
        checked += shown.length;
      }
    }
    expect(checked).toBeGreaterThan(0);
  });
});

describe('small helpers', () => {
  it('names only an explicit false as undefined', () => {
    expect(isUndefinedVlan({ defined: false })).toBe(true);
    expect(isUndefinedVlan({ defined: null })).toBe(false);
    expect(isUndefinedVlan({ defined: true })).toBe(false);
  });

  it('writes the gateway addresses with their prefix', () => {
    expect(
      gatewayAddresses({
        gateway: {
          if_name: 'Vl20',
          ifindex: 50,
          node_id: null,
          addresses: [
            { ip: '192.0.2.1', prefix_len: 24 },
            { ip: '2001:db8::1', prefix_len: null },
          ],
        },
      }),
    ).toBe('192.0.2.1/24, 2001:db8::1');
    expect(gatewayAddresses({ gateway: null })).toBe('');
  });
});
