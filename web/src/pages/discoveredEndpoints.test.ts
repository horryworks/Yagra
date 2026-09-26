// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  coverageOf,
  detectPhase,
  detectResultOf,
  detectedDevice,
  detectedSelection,
  importName,
  importNameAfterDetect,
  isUnmonitored,
  sourcesOf,
  DISCOVERY_TABS,
  ENDPOINT_COVERAGE,
  ENDPOINT_SOURCES,
} from './discoveredEndpoints';
import type { DiscoveredEndpoint, DiscoveryScan } from '../types/api';

function endpoint(over: Partial<DiscoveredEndpoint> = {}): DiscoveredEndpoint {
  return {
    id: '00000000-0000-0000-0000-000000000001',
    ip: '192.168.1.50',
    mac: 'aa:bb:cc:dd:ee:ff',
    via_node: 'n1',
    via_ifindex: 8,
    name: null,
    evidence: [{ source: 'arp', via_node: 'n1', via_ifindex: 8, port: null, detail: null }],
    first_seen: '2026-08-04T00:00:00Z',
    last_seen: '2026-08-04T01:00:00Z',
    promoted_node_id: null,
    ...over,
  } as DiscoveredEndpoint;
}

describe('coverageOf', () => {
  it('calls an empty list "off" when nothing has reported an ARP cache', () => {
    // The distinction the whole card turns on: "nobody looked" must never render as "nothing to
    // find". ARP discovery ships disabled, so this is the *default* state of every deployment.
    expect(coverageOf({ observed_total: 0, nodes_reporting: 0, truncated_nodes: 0, unmonitored_total: 0 })).toBe('off');
  });

  it('calls it complete when routers reported and none hit a cap', () => {
    expect(coverageOf({ observed_total: 41, nodes_reporting: 3, truncated_nodes: 0, unmonitored_total: 0 })).toBe(
      'complete',
    );
    // Reported, and genuinely nothing unmonitored — a clean bill of health, not silence.
    expect(coverageOf({ observed_total: 0, nodes_reporting: 3, truncated_nodes: 0, unmonitored_total: 0 })).toBe(
      'complete',
    );
  });

  it('calls it sampled as soon as one router hit its row budget', () => {
    // One truncated cache is enough: the list is a floor from that point on, and rounding that off
    // to "complete" would present a sample as an inventory.
    expect(coverageOf({ observed_total: 4096, nodes_reporting: 9, truncated_nodes: 1, unmonitored_total: 0 })).toBe(
      'sampled',
    );
  });

  it('degrades to off rather than crashing on a summary a server did not send', () => {
    expect(coverageOf(undefined as never)).toBe('off');
  });

  it('only ever returns a member of the declared set', () => {
    // The set is what the i18n coverage test iterates; a fourth value would render a raw key.
    for (const s of [
      { observed_total: 0, nodes_reporting: 0, truncated_nodes: 0, unmonitored_total: 0 },
      { observed_total: 1, nodes_reporting: 1, truncated_nodes: 0, unmonitored_total: 0 },
      { observed_total: 1, nodes_reporting: 1, truncated_nodes: 1, unmonitored_total: 0 },
    ]) {
      expect(ENDPOINT_COVERAGE).toContain(coverageOf(s));
    }
  });
});

describe('isUnmonitored', () => {
  it('flips the moment a row names the node it became', () => {
    expect(isUnmonitored(endpoint())).toBe(true);
    expect(isUnmonitored(endpoint({ promoted_node_id: 'n9' }))).toBe(false);
  });
});

describe('sourcesOf', () => {
  it("lists each source once, in the backend's order, whatever order the evidence came in", () => {
    const e = endpoint({
      evidence: [
        { source: 'syslog', via_node: null, via_ifindex: null, port: null, detail: 'fw-01' },
        { source: 'lldp', via_node: 'n2', via_ifindex: null, port: 'Gi1/0/1', detail: null },
        { source: 'arp', via_node: 'n1', via_ifindex: 8, port: null, detail: null },
        { source: 'arp', via_node: 'n3', via_ifindex: 2, port: null, detail: null },
      ],
    });
    expect(sourcesOf(e)).toEqual(['arp', 'lldp', 'syslog']);
  });

  it('knows every source the backend can send, in the order it sends them', () => {
    expect(ENDPOINT_SOURCES).toEqual(['arp', 'lldp', 'cdp', 'ospf', 'bgp', 'syslog', 'trap']);
  });
});

describe('importName', () => {
  it('prefills the reported name and leaves a blank one to the backend', () => {
    expect(importName(endpoint({ name: 'sw-07' }))).toBe('sw-07');
    expect(importName(endpoint({ name: '   ' }))).toBeUndefined();
    expect(importName(endpoint({ name: null }))).toBeUndefined();
  });
});

describe('DISCOVERY_TABS', () => {
  it('opens on the sweep form, so links made before the tabs existed land where they did', () => {
    expect(DISCOVERY_TABS[0]).toBe('scan');
  });
});

function scan(over: Partial<DiscoveryScan> = {}): DiscoveryScan {
  return {
    scan_id: 's1',
    done: true,
    state: 'done',
    probed: 1,
    total: 1,
    scanning: null,
    started_at: '2026-09-26T00:00:00Z',
    candidates: [],
    existing: [],
    ...over,
  } as DiscoveryScan;
}

const answered = {
  address: '192.0.2.44',
  reachable: true,
  sysdescr: 'Cisco IOS Software',
  sysname: 'sw-07',
  sysobjectid: '1.3.6.1.4.1.9.1.1',
  suggested_profile_id: 'p-cisco',
  vendor: 'Cisco',
  model: 'C2960X',
  matched_credential_id: 'c-corp',
};

describe('detectResultOf', () => {
  it('says nothing while the sweep is still running', () => {
    expect(detectResultOf(scan({ done: false, state: 'running' }), '192.0.2.44')).toBeNull();
  });

  it('reads the matched credential and the suggested profile as found', () => {
    expect(detectResultOf(scan({ candidates: [answered] }), '192.0.2.44')).toEqual({
      kind: 'found',
      profileId: 'p-cisco',
      credentialId: 'c-corp',
      vendor: 'Cisco',
      model: 'C2960X',
      sysname: 'sw-07',
    });
  });

  it('calls a host that only answered ping silent, not found', () => {
    const pingOnly = { ...answered, suggested_profile_id: null, matched_credential_id: null };
    expect(detectResultOf(scan({ candidates: [pingOnly] }), '192.0.2.44')).toEqual({ kind: 'silent' });
    expect(detectResultOf(scan(), '192.0.2.44')).toEqual({ kind: 'silent' });
  });

  it('does not read another address as this one', () => {
    expect(detectResultOf(scan({ candidates: [answered] }), '192.0.2.45')).toEqual({ kind: 'silent' });
  });

  it('says a cancelled sweep proved nothing', () => {
    expect(detectResultOf(scan({ state: 'cancelled' }), '192.0.2.44')).toEqual({ kind: 'lost' });
  });
});

describe('detected results', () => {
  const found = {
    kind: 'found' as const,
    profileId: 'p-cisco',
    credentialId: 'c-corp',
    vendor: 'Cisco',
    model: 'C2960X',
    sysname: 'sw-07',
  };

  it('names the device by maker and model, then by its own name', () => {
    expect(detectedDevice(found)).toBe('Cisco C2960X');
    expect(detectedDevice({ ...found, vendor: undefined, model: undefined })).toBe('sw-07');
    expect(detectedDevice({ ...found, vendor: undefined, model: undefined, sysname: undefined })).toBe('');
  });

  it('fills only ids the dropdowns can show', () => {
    expect(detectedSelection(found, ['p-cisco'], ['c-corp'])).toEqual({
      profile_id: 'p-cisco',
      credential_id: 'c-corp',
    });
    expect(detectedSelection(found, [], ['c-corp'])).toEqual({ profile_id: '', credential_id: 'c-corp' });
  });

  it('keeps the name a neighbour gave over the one the device reports', () => {
    expect(importNameAfterDetect(endpoint({ name: 'sw-edge' }), found)).toBe('sw-edge');
    expect(importNameAfterDetect(endpoint(), found)).toBe('sw-07');
    expect(importNameAfterDetect(endpoint(), { kind: 'silent' })).toBeUndefined();
    expect(importNameAfterDetect(endpoint(), undefined)).toBeUndefined();
  });
});

describe('detectPhase', () => {
  it('shows the dropdowns only once a Detect has answered', () => {
    expect(detectPhase(undefined)).toBe('idle');
    expect(detectPhase('running')).toBe('running');
    expect(
      detectPhase({ kind: 'found', profileId: 'p', credentialId: 'c' }),
    ).toBe('found');
  });

  it('turns both kinds of no-answer into picking by hand', () => {
    expect(detectPhase({ kind: 'silent' })).toBe('manual');
    expect(detectPhase({ kind: 'lost' })).toBe('manual');
  });
});
