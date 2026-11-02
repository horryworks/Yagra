// SPDX-License-Identifier: AGPL-3.0-only
// What each Interfaces column sorts on (ADR-228).
//
// Sorted in the browser, which `lib/tableSort.ts` allows only for a list whose every row is
// present: one device's ports are, and the tab already filters them client-side.
//
// Each value is what the cell MEANS, not what it renders: speed sorts on bps, not on "1G" beside
// "10G"; oper and duplex sort on their place in the closed list, so "up" comes first. A port with
// no value for a column sorts last in both directions (`sortRows`).
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import type { InterfaceRow, Neighbor } from '../../types/api';
import type { SortState, SortValues } from '../../lib/tableSort';
import { addressesOf, formatAddress } from './interfaceAddresses';
import { trafficCell } from './interfaceMetrics';
import { carriedVlanTokens, VLAN_MODE_KEYS, vlanModeKey } from './interfaceVlan';
import { DUPLEX_STATES, duplexState, mediaText } from './linkMode';
import { peerLabel } from './neighbors';
import { IF_STATES, ifState } from './tabFilters';

/** A row as the tab holds it: the device's row plus the VLAN facts its aggregate lends it. */
export type SortableInterface = InterfaceRow & {
  vlan_effective?: InterfaceRow['vlan'];
};

/** The header's order. Throughput is a sparkline, a picture of an hour, so it has no value. */
export const INTERFACE_SORT_KEYS = [
  'if_name',
  'oper',
  'if_alias',
  'addresses',
  'neighbors',
  'media',
  'speed',
  'duplex',
  'mode',
  'vlan',
  'in',
  'out',
] as const;

/** The order a device numbers its ports in, which is what the tab showed before it could sort. */
export const DEFAULT_INTERFACE_SORT: SortState = { by: 'if_name', dir: 'asc' };

/** A closed list's position, with its "nothing known" member read as missing so it sorts last. */
function rank<K extends string>(list: readonly K[], key: K, unknown: K): number | null {
  return key === unknown ? null : list.indexOf(key);
}

export function interfaceSortValues(
  neighborsOf: (ifindex: number) => readonly Neighbor[] | undefined,
): SortValues<SortableInterface> {
  return {
    // The order the device numbers its ports in, which is also the order the tab always showed.
    // A name sort puts `Vlan10` among the `Te` ports and `GE0/0/10` before `GE0/0/2` on devices
    // that pad nothing — the ifIndex is the order an operator reads a switch in.
    if_name: (r) => r.ifindex,
    oper: (r) => rank(IF_STATES, ifState(r.oper_status), 'unknown'),
    if_alias: (r) => r.if_alias,
    addresses: (r) => {
      const first = addressesOf(r)[0];
      return first ? formatAddress(first) : null;
    },
    neighbors: (r) => {
      const first = neighborsOf(r.ifindex)?.[0];
      return first ? peerLabel(first) : null;
    },
    media: (r) => mediaText(r.if_media) ?? r.transceiver_model,
    speed: (r) => (r.if_speed_bps != null && r.if_speed_bps > 0 ? r.if_speed_bps : null),
    duplex: (r) => rank(DUPLEX_STATES, duplexState(r.if_duplex), 'unknown'),
    mode: (r) => rank(VLAN_MODE_KEYS, vlanModeKey(r), 'not_reported'),
    vlan: (r) => {
      const first = carriedVlanTokens(r.vlan_effective ?? r.vlan)[0];
      return first === undefined ? null : Number(first);
    },
    in: (r) => trafficCell(r, 'in')?.bps ?? null,
    out: (r) => trafficCell(r, 'out')?.bps ?? null,
  };
}
