// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { coverageOf, isUnmonitored, ENDPOINT_COVERAGE } from './discoveredEndpoints';
import type { DiscoveredEndpoint } from '../types/api';

function endpoint(over: Partial<DiscoveredEndpoint> = {}): DiscoveredEndpoint {
  return {
    id: '00000000-0000-0000-0000-000000000001',
    ip: '192.168.1.50',
    mac: 'aa:bb:cc:dd:ee:ff',
    via_node: 'n1',
    via_ifindex: 8,
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
    expect(coverageOf({ observed_total: 0, nodes_reporting: 0, truncated_nodes: 0 })).toBe('off');
  });

  it('calls it complete when routers reported and none hit a cap', () => {
    expect(coverageOf({ observed_total: 41, nodes_reporting: 3, truncated_nodes: 0 })).toBe(
      'complete',
    );
    // Reported, and genuinely nothing unmonitored — a clean bill of health, not silence.
    expect(coverageOf({ observed_total: 0, nodes_reporting: 3, truncated_nodes: 0 })).toBe(
      'complete',
    );
  });

  it('calls it sampled as soon as one router hit its row budget', () => {
    // One truncated cache is enough: the list is a floor from that point on, and rounding that off
    // to "complete" would present a sample as an inventory.
    expect(coverageOf({ observed_total: 4096, nodes_reporting: 9, truncated_nodes: 1 })).toBe(
      'sampled',
    );
  });

  it('degrades to off rather than crashing on a summary a server did not send', () => {
    expect(coverageOf(undefined as never)).toBe('off');
  });

  it('only ever returns a member of the declared set', () => {
    // The set is what the i18n coverage test iterates; a fourth value would render a raw key.
    for (const s of [
      { observed_total: 0, nodes_reporting: 0, truncated_nodes: 0 },
      { observed_total: 1, nodes_reporting: 1, truncated_nodes: 0 },
      { observed_total: 1, nodes_reporting: 1, truncated_nodes: 1 },
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
