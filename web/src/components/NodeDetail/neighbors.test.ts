// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  diffNeighbors,
  emptyReason,
  neighborKey,
  peerLabel,
  peerLabelIsChassis,
} from './neighbors';
import type { Neighbor, NeighborSet } from '../../types/api';

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
