// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  FACT_ROWS,
  FACT_ROWS_BY_KIND,
  overviewShowsIcmp,
  visibleFactRows,
} from './overviewFacts';
import { NODE_KINDS } from '../../types/api';

// The regression these pin: a DNS monitor's Overview led with "ICMP RTT · last 30 min /
// No RTT history yet…" and a facts grid whose Maker, Model, SNMP credential and Uptime were all an
// em dash — four rows describing an SNMP walk that is never performed on that kind, and an empty
// chart for a protocol it is never probed with.
describe('overview fact rows', () => {
  it('covers exactly the backend node kinds', () => {
    expect(Object.keys(FACT_ROWS_BY_KIND).sort()).toEqual([...NODE_KINDS].sort());
  });

  it('lists only known rows, without duplicates, and uses every row somewhere', () => {
    const used = new Set<string>();
    for (const kind of NODE_KINDS) {
      const rows = FACT_ROWS_BY_KIND[kind];
      expect(rows.length, kind).toBeGreaterThan(0);
      expect(new Set(rows).size, kind).toBe(rows.length);
      for (const r of rows) {
        expect(FACT_ROWS, kind).toContain(r);
        used.add(r);
      }
    }
    expect([...used].sort()).toEqual([...FACT_ROWS].sort());
  });

  it('pins the per-kind fact rows', () => {
    expect([...visibleFactRows('device')]).toEqual([
      'group',
      'pool',
      'polledBy',
      'address',
      'maker',
      'model',
      'profile',
      'credential',
      'parent',
      'uptime',
    ]);
    expect([...visibleFactRows('meraki')]).toEqual([
      'group',
      'pool',
      'polledBy',
      'address',
      'maker',
      'model',
      'profile',
      'credential',
      'parent',
    ]);
    expect([...visibleFactRows('url')]).toEqual(['group', 'pool', 'polledBy', 'profile', 'parent']);
    expect([...visibleFactRows('dns')]).toEqual([
      'group',
      'pool',
      'polledBy',
      'profile',
      'parent',
      'resolver',
    ]);
  });

  // Where a node is filed and which poller owns it are true of a monitor exactly as of a switch.
  it('keeps the placement facts for every kind', () => {
    for (const kind of NODE_KINDS) {
      const rows = visibleFactRows(kind);
      for (const r of ['group', 'pool', 'polledBy', 'profile', 'parent'] as const) {
        expect(rows, `${kind}/${r}`).toContain(r);
      }
    }
  });

  it('drops the SNMP-derived rows from the kinds that are never SNMP-walked', () => {
    for (const kind of ['url', 'dns'] as const) {
      const rows = visibleFactRows(kind);
      for (const r of ['maker', 'model', 'credential', 'uptime', 'address'] as const) {
        expect(rows, `${kind}/${r}`).not.toContain(r);
      }
    }
    // A Meraki device has a real management address and reported maker/model, but no sysUpTime.
    expect(visibleFactRows('meraki')).not.toContain('uptime');
    expect(visibleFactRows('meraki')).toContain('address');
    // Only the DNS monitor names its resolver.
    for (const kind of NODE_KINDS) {
      expect(visibleFactRows(kind).includes('resolver'), kind).toBe(kind === 'dns');
    }
  });

  // Order comes from FACT_ROWS, so no kind can reshuffle the grid relative to another.
  it('renders rows in the registry order, not the per-kind order', () => {
    for (const kind of NODE_KINDS) {
      const positions = visibleFactRows(kind).map((r) => FACT_ROWS.indexOf(r));
      expect(positions, kind).toEqual([...positions].sort((a, b) => a - b));
    }
  });
});

describe('overviewShowsIcmp', () => {
  // `scheduler/assemble.rs::assemble_node_jobs` returns before the ICMP branch for a URL or DNS monitor,
  // and a Meraki node emits no per-node job at all — `icmp_rtt_ms` is structurally absent for
  // three of the four kinds, not merely empty right now.
  it('shows the ICMP sparkline only for an ordinary device', () => {
    for (const kind of NODE_KINDS) {
      expect(overviewShowsIcmp(kind), kind).toBe(kind === 'device');
    }
  });
});
