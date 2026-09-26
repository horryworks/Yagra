// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  chassisVendor,
  diffNeighbors,
  emptyReason,
  NEIGHBOR_DETAIL_KEYS,
  neighborAddressState,
  neighborCellText,
  neighborDetails,
  neighborKey,
  neighborLookups,
  neighborsByPort,
  NO_LOOKUPS,
  peerLabel,
  peerLabelIsChassis,
  peerNodePath,
  peerOf,
  peerSecondary,
  platformCell,
  portVendor,
  merakiOrgPath,
  setupMode,
  setupName,
} from './neighbors';
import type { Neighbor, NeighborPeer, NeighborSet } from '../../types/api';

function n(over: Partial<Neighbor> = {}): Neighbor {
  return {
    proto: 'lldp',
    local_port: 'Gi0/1',
    remote_chassis: 'aa:bb:cc:dd:ee:01',
    remote_port: 'Gi1/0/24',
    local_ifindex: null,
    remote_port_desc: null,
    remote_sys_name: 'core-sw-01',
    remote_sys_desc: null,
    remote_mgmt_addr: null,
    remote_platform: null,
    capabilities: ['router', 'bridge'],
    ...over,
  } as Neighbor;
}

function set(...neighbors: Neighbor[]): NeighborSet {
  return { neighbors, truncated: false } as NeighborSet;
}

describe('diffNeighbors', () => {
  it('reports nothing when the adjacency is unchanged', () => {
    expect(diffNeighbors(set(n()), set(n()))).toEqual([]);
  });

  it('treats every adjacency as added when there is no previous observation', () => {
    // The genesis row: nothing existed before, so every link is new rather than "added" relative
    // to some earlier state.
    const rows = diffNeighbors(null, set(n(), n({ local_port: 'Gi0/2' })));
    expect(rows.map((r) => r.kind)).toEqual(['added', 'added']);
  });

  it('reports an added link', () => {
    const rows = diffNeighbors(set(n()), set(n(), n({ local_port: 'Gi0/2' })));
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe('added');
    expect(rows[0].neighbor.local_port).toBe('Gi0/2');
  });

  it('reports a removed link', () => {
    const rows = diffNeighbors(set(n(), n({ local_port: 'Gi0/2' })), set(n()));
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe('removed');
    expect(rows[0].neighbor.local_port).toBe('Gi0/2');
  });

  it('reports a repatched port as a remove plus an add, not a change', () => {
    // The peer on Gi0/1 is a different device: its chassis id is part of the identity, so this is
    // one link ending and another starting — describing it as "changed" would hide the disconnect.
    const rows = diffNeighbors(set(n()), set(n({ remote_chassis: 'aa:bb:cc:dd:ee:09' })));
    expect(rows.map((r) => r.kind).sort()).toEqual(['added', 'removed']);
  });

  it('reports a renamed or reimaged peer as changed, keeping the link', () => {
    const rows = diffNeighbors(set(n()), set(n({ remote_sys_name: 'core-sw-01-replaced' })));
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe('changed');
  });

  it('ignores capability ordering', () => {
    // The backend sorts capabilities before storing, but a reordering must not read as a change
    // even if a future producer does not.
    const rows = diffNeighbors(set(n()), set(n({ capabilities: ['bridge', 'router'] })));
    expect(rows).toEqual([]);
  });

  it('keeps the same link seen by both protocols separate', () => {
    // A switch running both reports one physical link twice under different identities; treating
    // them as one would make enabling CDP look like every LLDP peer had been replaced.
    const lldp = n();
    const cdp = n({ proto: 'cdp', remote_chassis: 'core-sw-01' });
    const rows = diffNeighbors(set(lldp), set(lldp, cdp));
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe('added');
    expect(rows[0].neighbor.proto).toBe('cdp');
  });

  it('handles an adjacency disappearing entirely', () => {
    const rows = diffNeighbors(set(n(), n({ local_port: 'Gi0/2' })), set());
    expect(rows.map((r) => r.kind)).toEqual(['removed', 'removed']);
  });
});

describe('emptyReason', () => {
  it('distinguishes never-recorded from genuinely-none', () => {
    // These look identical on screen but mean opposite things: one is "we have not looked", the
    // other is "we looked and this device has no neighbours".
    expect(emptyReason(true, null)).toBe('unrecorded');
    expect(emptyReason(true, { neighbors: set() })).toBe('none');
  });

  it('names the deployment-wide switch when nothing was ever recorded', () => {
    expect(emptyReason(false, null)).toBe('disabled');
  });

  it('still reports recorded data after collection is switched off', () => {
    // Turning collection off does not delete history, so the tab keeps showing what it has.
    expect(emptyReason(false, { neighbors: set(n()) })).toBeNull();
    // A recorded empty set stays "none" rather than becoming "disabled" — the walk did happen.
    expect(emptyReason(false, { neighbors: set() })).toBe('none');
  });

  it('reports nothing to explain when there are neighbours', () => {
    expect(emptyReason(true, { neighbors: set(n()) })).toBeNull();
  });
});

describe('labels', () => {
  it('prefers the peer system name over its chassis id', () => {
    expect(peerLabel(n())).toBe('core-sw-01');
    expect(peerLabelIsChassis(n())).toBe(false);
  });

  it('falls back to the chassis id when the peer published no name', () => {
    // LLDP peers often publish only a MAC. Accurate, but the UI should not print it twice.
    expect(peerLabel(n({ remote_sys_name: null }))).toBe('aa:bb:cc:dd:ee:01');
    expect(peerLabelIsChassis(n({ remote_sys_name: null }))).toBe(true);
    expect(peerLabel(n({ remote_sys_name: '   ' }))).toBe('aa:bb:cc:dd:ee:01');
  });

  it('keys rows by identity so two links on one port stay distinct', () => {
    expect(neighborKey(n())).not.toBe(neighborKey(n({ remote_port: 'Gi1/0/25' })));
    expect(neighborKey(n())).toBe(neighborKey(n({ remote_sys_name: 'renamed' })));
  });
});

describe('neighborsByPort', () => {
  const ports = [
    { ifindex: 1, if_name: 'Gi0/1' },
    { ifindex: 2, if_name: 'Gi0/2' },
    { ifindex: 7, if_name: 'GigabitEthernet0/7' },
  ];

  it('places a CDP neighbour by its ifIndex, whatever it calls the port', () => {
    // CDP falls back to `ifindex <n>` when the naming table has no row, so the name is not evidence.
    const cdp = n({ proto: 'cdp', local_port: 'ifindex 7', local_ifindex: 7 });
    expect(neighborsByPort([cdp], ports).get(7)).toEqual([cdp]);
  });

  it('does not place a CDP neighbour by name when its ifIndex names another port', () => {
    // The accepting case above could pass by name; this one names Gi0/1 and carries ifIndex 2.
    const cdp = n({ proto: 'cdp', local_port: 'Gi0/1', local_ifindex: 2 });
    const by = neighborsByPort([cdp], ports);
    expect(by.get(1)).toBeUndefined();
    expect(by.get(2)).toEqual([cdp]);
  });

  it('drops a CDP neighbour whose ifIndex is not in the list', () => {
    const cdp = n({ proto: 'cdp', local_port: 'Gi0/1', local_ifindex: 99 });
    expect([...neighborsByPort([cdp], ports).keys()]).toEqual([]);
  });

  it('places an LLDP neighbour by name, ignoring case and surrounding space', () => {
    const lldp = n({ local_port: '  gi0/2 ' });
    expect(neighborsByPort([lldp], ports).get(2)).toEqual([lldp]);
  });

  it('does not place an LLDP neighbour whose port name is spelled differently', () => {
    // The known limit, pinned so nobody "fixes" it by accident into guessing: a short name against
    // a long one, and Junos's bare number against its ifIndex (ADR-145 決定 2).
    const short = n({ local_port: 'Gi0/7' });
    const numeric = n({ local_port: '7', remote_chassis: 'aa:bb:cc:dd:ee:07' });
    expect([...neighborsByPort([short, numeric], ports).keys()]).toEqual([]);
  });

  it('places nothing on a name two rows share', () => {
    const dup = [
      { ifindex: 1, if_name: 'port' },
      { ifindex: 2, if_name: 'PORT' },
    ];
    expect([...neighborsByPort([n({ local_port: 'port' })], dup).keys()]).toEqual([]);
  });

  it('keeps several neighbours on one port, in the order the server sent them', () => {
    const a = n({ remote_sys_name: 'a' });
    const b = n({ remote_chassis: 'aa:bb:cc:dd:ee:02', remote_sys_name: 'b' });
    expect(neighborsByPort([a, b], ports).get(1)).toEqual([a, b]);
  });
});

describe('neighborCellText', () => {
  it('is null for a port with no neighbour', () => {
    expect(neighborCellText(undefined)).toBeNull();
    expect(neighborCellText([])).toBeNull();
  });

  it('shows the first peer and counts the rest', () => {
    const cell = neighborCellText([
      n(),
      n({ remote_chassis: 'aa:bb:cc:dd:ee:02', remote_sys_name: null, remote_port: '' }),
    ]);
    expect(cell).toEqual({
      label: 'core-sw-01',
      more: 1,
      // Every neighbour is in the title, because the cell itself ellipsizes.
      title: 'core-sw-01 Gi1/0/24\naa:bb:cc:dd:ee:02',
    });
  });
});

// ── ADR-180: what the tab adds to a row ─────────────────────────────────────────────────────────

const peer = (over: Partial<NeighborPeer> = {}): NeighborPeer =>
  ({
    address: '192.0.2.1',
    state: 'node',
    node_id: 'n-1',
    node_name: 'rtr-a',
    discovery_listed: false,
    ...over,
  }) as NeighborPeer;

const CISCO = { mac: '00:00:0c:12:34:56', vendor: 'Cisco Systems, Inc' };

describe('the lookups built from one response', () => {
  const lookups = neighborLookups({
    peers: [
      peer(),
      peer({
        address: '192.0.2.9',
        state: 'unregistered',
        node_id: null,
        node_name: null,
        discovery_listed: true,
      }),
      peer({ address: '192.0.2.5', state: 'outside_scope', node_id: null, node_name: null }),
    ],
    mac_vendors: [CISCO],
  });

  it('reads an absent response, or an older core without the lists, as nothing known', () => {
    expect(neighborLookups(null)).toBe(NO_LOOKUPS);
    const old = neighborLookups({} as Parameters<typeof neighborLookups>[0]);
    expect(old.peers.size).toBe(0);
    expect(old.vendors.size).toBe(0);
  });

  it('finds the verdict for a row by the address text it carries', () => {
    expect(peerOf(n({ remote_mgmt_addr: '192.0.2.1' }), lookups)?.node_name).toBe('rtr-a');
    expect(peerOf(n({ remote_mgmt_addr: null }), lookups)).toBeNull();
  });

  it('names the address state, and "none" only when the neighbour advertised no address', () => {
    expect(neighborAddressState(n({ remote_mgmt_addr: '192.0.2.5' }), lookups)).toBe(
      'outside_scope',
    );
    expect(neighborAddressState(n({ remote_mgmt_addr: null }), lookups)).toBe('none');
    // An address the server did not classify is no state, never a guessed one.
    expect(neighborAddressState(n({ remote_mgmt_addr: '192.0.2.77' }), NO_LOOKUPS)).toBeNull();
  });

  it('gives a maker only to an id the device labelled a MAC', () => {
    const mac = n({ remote_chassis: CISCO.mac, remote_chassis_kind: 'mac' });
    expect(chassisVendor(mac, lookups)).toBe(CISCO.vendor);
    // The same text, labelled otherwise, borrows nothing.
    expect(chassisVendor({ ...mac, remote_chassis_kind: 'text' }, lookups)).toBeNull();
    expect(chassisVendor({ ...mac, remote_chassis_kind: null }, lookups)).toBeNull();
    const port = n({ remote_port: CISCO.mac, remote_port_kind: 'mac' });
    expect(portVendor(port, lookups)).toBe(CISCO.vendor);
  });

  it('puts the chassis id and its maker under a name, and only the maker under a bare chassis', () => {
    const named = n({ remote_chassis: CISCO.mac, remote_chassis_kind: 'mac' });
    expect(peerSecondary(named, lookups)).toBe(`${CISCO.mac} · ${CISCO.vendor}`);
    expect(peerSecondary({ ...named, remote_sys_name: null }, lookups)).toBe(CISCO.vendor);
    expect(peerSecondary(n({ remote_sys_name: null }), NO_LOOKUPS)).toBeNull();
  });

  it('links a peer only when exactly one visible node owns its address', () => {
    expect(peerNodePath(peer())).toBe('/nodes/n-1');
    expect(peerNodePath(peer({ state: 'outside_scope', node_id: null }))).toBeNull();
    expect(peerNodePath(peer({ state: 'ambiguous', node_id: null }))).toBeNull();
    expect(peerNodePath(null)).toBeNull();
  });
});

describe('the model / OS cell', () => {
  it('leads with the CDP platform and puts its version banner underneath', () => {
    const cdp = n({ proto: 'cdp', remote_platform: 'cisco WS-C2960', remote_sys_desc: 'IOS 15.0' });
    expect(platformCell(cdp)).toEqual({ primary: 'cisco WS-C2960', secondary: 'IOS 15.0' });
  });

  it('shows the LLDP description alone, and nothing when neither was sent', () => {
    expect(platformCell(n({ remote_sys_desc: 'Linux 6.1' }))).toEqual({
      primary: 'Linux 6.1',
      secondary: null,
    });
    expect(platformCell(n({ remote_sys_desc: '  ' }))).toEqual({ primary: null, secondary: null });
  });
});

describe('the opened row', () => {
  it('lists every field the device sent, in reading order, and leaves out the rest', () => {
    const row = n({
      remote_chassis: CISCO.mac,
      remote_chassis_kind: 'mac',
      remote_mgmt_addr: '192.0.2.1',
      remote_sys_desc: 'Cisco IOS Software, Version 15.2',
      local_ifindex: 7,
    });
    const lookups = neighborLookups({ peers: [], mac_vendors: [CISCO] });
    const details = neighborDetails(row, lookups);
    expect(details.map((d) => d.labelKey)).toEqual([
      'sysName',
      'chassis',
      'chassisVendor',
      'port',
      'mgmtAddr',
      'sysDesc',
      'localIfindex',
    ]);
    expect(details.find((d) => d.labelKey === 'localIfindex')?.value).toBe('7');
  });

  it('has no repeated detail key', () => {
    expect(new Set(NEIGHBOR_DETAIL_KEYS).size).toBe(NEIGHBOR_DETAIL_KEYS.length);
  });

  it('adds nothing that counts as a change: the id kind is not part of the payload', () => {
    const after = set(n({ remote_chassis_kind: 'mac', remote_port_kind: 'text' }));
    expect(diffNeighbors(set(n()), after)).toEqual([]);
  });
});

// ── ADR-179 増分 3: adding an unmonitored neighbour from the tab ────────────────────────────────

describe('which setup a row offers', () => {
  const at = (p: Partial<NeighborPeer>) =>
    neighborLookups({
      peers: [peer({ address: '192.0.2.9', state: 'unregistered', node_id: null, node_name: null, ...p })],
      mac_vendors: [],
    });
  const row = (over: Partial<Neighbor> = {}) => n({ remote_mgmt_addr: '192.0.2.9', ...over });
  const listed = { discovery_listed: true, discovery_id: 'd-1' };

  it('sends an AP a visible controller reports to that controller, not to Detect', () => {
    const lookups = at({
      ...listed,
      managed_by: {
        kind: 'controller',
        ap_id: 'ap-1',
        controller_node_id: 'wlc-1',
        controller_name: 'wlc01',
        imported: false,
      },
    });
    expect(setupMode(row({ capabilities: ['wlan_ap'] }), lookups)).toEqual({
      kind: 'controller',
      apId: 'ap-1',
      controllerId: 'wlc-1',
      controllerName: 'wlc01',
      imported: false,
    });
  });

  it('sends a device a Meraki organization lists to that organization', () => {
    const lookups = at({ ...listed, managed_by: { kind: 'meraki', org_id: 'o-1', org_name: 'Acme' } });
    expect(setupMode(row(), lookups)).toEqual({ kind: 'meraki', orgId: 'o-1', orgName: 'Acme' });
  });

  it('offers nothing for an AP only a controller outside the caller’s folders reports', () => {
    const lookups = at({ ...listed, managed_by: { kind: 'controller_hidden' } });
    expect(setupMode(row({ capabilities: ['wlan_ap'] }), lookups)).toBeNull();
  });

  it('runs Detect for a self-declared AP nothing manages, as a standalone AP', () => {
    expect(setupMode(row({ capabilities: ['wlan_ap'] }), at(listed))).toEqual({
      kind: 'standalone_ap',
      discoveryId: 'd-1',
    });
    // A router with a radio says both, and nothing manages it: still the AP path.
    expect(setupMode(row({ capabilities: ['router', 'wlan_ap'] }), at(listed))?.kind).toBe(
      'standalone_ap',
    );
  });

  it('runs Discovery’s own flow for any other device', () => {
    expect(setupMode(row(), at(listed))).toEqual({ kind: 'device', discoveryId: 'd-1' });
  });

  it('offers nothing where there is nothing to act on', () => {
    // Monitored, or owned outside the caller's scope.
    expect(setupMode(row(), at({ ...listed, state: 'node', node_id: 'n-1' }))).toBeNull();
    expect(setupMode(row(), at({ ...listed, state: 'outside_scope' }))).toBeNull();
    // Not on the Unregistered list, or listed by an older core that sends no id.
    expect(setupMode(row(), at({ discovery_listed: false }))).toBeNull();
    expect(setupMode(row(), at({ discovery_listed: true }))).toBeNull();
    // No address advertised.
    expect(setupMode(n(), at(listed))).toBeNull();
  });
});

describe('the name a neighbour is added under', () => {
  it('is its system name, never its chassis id', () => {
    expect(setupName(n({ remote_sys_name: 'sw-07' }))).toBe('sw-07');
    expect(setupName(n({ remote_sys_name: null }))).toBeNull();
    expect(setupName(n({ remote_sys_name: '  ' }))).toBeNull();
  });

  it('links a Meraki organization to its page', () => {
    expect(merakiOrgPath('o-1')).toBe('/settings/integrations/meraki/o-1');
  });
});
