// SPDX-License-Identifier: AGPL-3.0-only
// What the VLANs tab works out in the browser (ADR-257 decision 7): how many ports carry each VLAN,
// and the Interfaces view a count opens.
//
// Kept out of `VlansTab.tsx` so it runs under Vitest (a `.tsx` never does). The counts are taken
// with the Interfaces tab's own predicates — the MODE filter's `vlanModeKey` and the VLAN filter's
// `carriedVlanTokens(effectiveVlan(...))` — so pressing a count opens exactly that many rows.

import { encodeSet } from '../../lib/columnFilter';
import type { InterfaceRow, NodeVlan } from '../../types/api';
import {
  carriedVlanTokens,
  effectiveVlan,
  VLAN_MODE_KEYS,
  vlanModeKey,
  type VlanModeKey,
} from './interfaceVlan';
import { nodeTabFilterPrefix } from './tabs';

/** The two counts a VLAN row shows, in column order. */
export const PORT_GROUPS = ['access', 'trunk'] as const;
export type PortGroup = (typeof PORT_GROUPS)[number];

/** Which MODE filter values each count stands for. A hybrid port carries tagged VLANs, so it is
 *  counted with the trunks; a member answers through its aggregate and is counted there. */
export const GROUP_MODES: Record<PortGroup, readonly VlanModeKey[]> = {
  access: ['access'],
  trunk: ['trunk', 'hybrid'],
};

export type PortCounts = Record<PortGroup, number>;

/** Per VLAN, how many ports carry it, split by [`PORT_GROUPS`]. A VLAN no port carries is absent. */
export function vlanPortCounts(rows: readonly InterfaceRow[]): Map<number, PortCounts> {
  const byIfindex = new Map(rows.map((r) => [r.ifindex, r] as const));
  const out = new Map<number, PortCounts>();
  for (const r of rows) {
    const mode = vlanModeKey(r);
    const group = PORT_GROUPS.find((g) => GROUP_MODES[g].includes(mode));
    if (!group) continue;
    for (const token of carriedVlanTokens(effectiveVlan(r, byIfindex))) {
      const id = Number(token);
      const c = out.get(id) ?? { access: 0, trunk: 0 };
      c[group] += 1;
      out.set(id, c);
    }
  }
  return out;
}

/** The query string that opens the Interfaces tab filtered to the ports one count stands for.
 *  Every other Interfaces filter is dropped, so the list shows exactly the counted rows; the rest of
 *  the URL (the selected node, the tree's own filters) is kept. One write, one URL. */
export function interfacesSearch(
  current: URLSearchParams,
  vlanId: number,
  group: PortGroup,
): string {
  const next = new URLSearchParams(current);
  const prefix = nodeTabFilterPrefix('interfaces');
  for (const key of [...next.keys()]) if (key.startsWith(prefix)) next.delete(key);
  next.set('tab', 'interfaces');
  next.set(`${prefix}vlan`, String(vlanId));
  next.set(`${prefix}mode`, encodeSet(GROUP_MODES[group], VLAN_MODE_KEYS));
  return `?${next.toString()}`;
}

/** Whether a VLAN row warns: a port uses it and the device's own VLAN table does not list it. */
export function isUndefinedVlan(v: Pick<NodeVlan, 'defined'>): boolean {
  return v.defined === false;
}

/** The gateway's addresses as one line, `10.0.20.1/23, 2001:db8::1/64`. */
export function gatewayAddresses(v: Pick<NodeVlan, 'gateway'>): string {
  return (v.gateway?.addresses ?? [])
    .map((a) => (a.prefix_len == null ? a.ip : `${a.ip}/${a.prefix_len}`))
    .join(', ');
}
