// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  NODE_DETAIL_TAB_META,
  NODE_DETAIL_TABS,
  normalizeNodeDetailTab,
  resolveNodeDetailTab,
  visibleNodeDetailTabs,
  type NodeDetailSubject,
  type NodeDetailTabStats,
} from './tabs';
import { NODE_KINDS, type InterfaceRow } from '../../types/api';

// Regression guard for the ADR-031 Flow-tab bug: the node-detail tab whitelist was duplicated
// across NodeDetail.tsx, NodeDetailPage.tsx, and NodesPage.tsx (the split-view host). The
// split-view copy was missed when Flow was added, so the Flow button rendered but clicking it
// bounced back to Overview. All three now import NODE_DETAIL_TABS, and the tab bar + body inside
// NodeDetail.tsx are `Record<NodeDetailTab, …>` maps, so the compiler rejects a half-added tab.
// These tests pin the shared source, the normalization, and the badge/warn rules the bar reads.
describe('node-detail tabs', () => {
  it('whitelists every rendered tab, including flow', () => {
    expect([...NODE_DETAIL_TABS]).toEqual([
      'overview',
      'interfaces',
      'neighbors',
      'collection',
      'events',
      'flow',
    ]);
    expect(NODE_DETAIL_TABS).toContain('flow');
  });

  it('normalizes a known tab to itself (so the split view keeps it selected)', () => {
    for (const tab of NODE_DETAIL_TABS) {
      expect(normalizeNodeDetailTab(tab)).toBe(tab);
    }
  });

  it('falls back to overview for unknown, empty, or wrong-case tabs', () => {
    expect(normalizeNodeDetailTab('')).toBe('overview');
    expect(normalizeNodeDetailTab('bogus')).toBe('overview');
    expect(normalizeNodeDetailTab('FLOW')).toBe('overview');
  });

  it('gives every whitelisted tab a label key, and defines no orphan entries', () => {
    expect(Object.keys(NODE_DETAIL_TAB_META).sort()).toEqual([...NODE_DETAIL_TABS].sort());
    const keys = Object.values(NODE_DETAIL_TAB_META).map((m) => m.labelKey);
    expect(new Set(keys).size).toBe(keys.length);
    for (const k of keys) expect(k).toMatch(/^tabs\./);
  });
});

// Which nodes see which tabs. A URL or DNS monitor produces one HTTP/DNS job and nothing else
// (scheduler/assemble.rs::assemble_node_jobs returns before the ICMP and SNMP branches), and a Meraki node
// emits no per-node job at all — so Interfaces / Neighbors / Flow are structurally unreachable for
// them, not merely empty today. A ping-only device is the same argument on the other axis: it is a
// device, but with no SNMP auth resolved it gets an ICMP job and nothing else (ADR-119).

/** Every node the rules can be asked about: each kind, with SNMP configured and without.
 *
 *  🚨 Iterating `NODE_KINDS` alone is what these structural tests used to do, and it leaves the
 *  ping-only device — the whole case ADR-119 exists for — unexercised while every one of them
 *  still passes. Cross the two axes here, once, so no test below can forget the second. */
const SUBJECTS: NodeDetailSubject[] = NODE_KINDS.flatMap((kind) => [
  { kind, snmpConfigured: true },
  { kind, snmpConfigured: false },
]);
const label = (n: NodeDetailSubject) => `${n.kind}/${n.snmpConfigured ? 'snmp' : 'ping-only'}`;

describe('node-detail tab visibility', () => {
  it('gives every whitelisted tab a non-empty list of real node kinds', () => {
    for (const tab of NODE_DETAIL_TABS) {
      const kinds = NODE_DETAIL_TAB_META[tab].kinds;
      expect(kinds.length, tab).toBeGreaterThan(0);
      for (const k of kinds) expect(NODE_KINDS, tab).toContain(k);
      expect(new Set(kinds).size, tab).toBe(kinds.length);
    }
  });

  // The recognition test for the second axis. Without it every assertion here is satisfiable by a
  // `needsSnmp` nothing reads — which is exactly what an under-reporting check looks like.
  it('actually narrows a device when SNMP is not configured', () => {
    const withSnmp = visibleNodeDetailTabs({ kind: 'device', snmpConfigured: true });
    const without = visibleNodeDetailTabs({ kind: 'device', snmpConfigured: false });
    expect(without.length).toBeLessThan(withSnmp.length);
    for (const tab of without) expect(withSnmp, tab).toContain(tab);
    expect(NODE_DETAIL_TABS.filter((t) => NODE_DETAIL_TAB_META[t].needsSnmp).length).toBeGreaterThan(
      0,
    );
  });

  // Load-bearing: 'overview' being visible everywhere is what makes resolveNodeDetailTab's
  // fallback terminate, and what makes NodesPage's "a fresh selection starts on Overview" path
  // (it deletes the ?tab= param) valid for every node. Do not narrow it.
  it('shows every node at least the overview tab', () => {
    for (const n of SUBJECTS) {
      expect(visibleNodeDetailTabs(n), label(n)).toContain('overview');
    }
  });

  // An orphan tab would be a dead body element and a dead locale key that i18n parity still passes.
  it('shows every tab to at least one node', () => {
    const shown = new Set(SUBJECTS.flatMap((n) => visibleNodeDetailTabs(n)));
    expect([...shown].sort()).toEqual([...NODE_DETAIL_TABS].sort());
  });

  it('pins the node → tab matrix', () => {
    const tabs = (kind: NodeDetailSubject['kind'], snmpConfigured: boolean) => [
      ...visibleNodeDetailTabs({ kind, snmpConfigured }),
    ];
    expect(tabs('device', true)).toEqual([...NODE_DETAIL_TABS]);
    // The ADR-119 case: Interfaces and Neighbors go, Events and Flow stay — they are attributed by
    // the device's address, so a ping-only node can have rows in either.
    expect(tabs('device', false)).toEqual(['overview', 'collection', 'events', 'flow']);
    expect(tabs('url', true)).toEqual(['overview', 'collection']);
    expect(tabs('url', false)).toEqual(['overview', 'collection']);
    expect(tabs('dns', false)).toEqual(['overview', 'collection']);
    expect(tabs('meraki', false)).toEqual(['overview', 'collection', 'events']);
  });

  // The bar's order comes from NODE_DETAIL_TABS, never from a per-node list, so nothing can
  // reshuffle the tabs relative to anything else.
  it('keeps the registry order for every node', () => {
    for (const n of SUBJECTS) {
      const visible = visibleNodeDetailTabs(n);
      const positions = visible.map((tab) => NODE_DETAIL_TABS.indexOf(tab));
      expect(positions, label(n)).toEqual([...positions].sort((a, b) => a - b));
    }
  });
});

describe('resolveNodeDetailTab', () => {
  it('is node-blind while the node is still unknown', () => {
    for (const tab of NODE_DETAIL_TABS) expect(resolveNodeDetailTab(tab, null)).toBe(tab);
    expect(resolveNodeDetailTab('bogus', null)).toBe('overview');
  });

  it('resolves every tab a node does show to itself', () => {
    for (const n of SUBJECTS) {
      for (const tab of visibleNodeDetailTabs(n)) {
        expect(resolveNodeDetailTab(tab, n), `${label(n)}/${tab}`).toBe(tab);
      }
    }
  });

  // The bookmarked-URL case: without this, `?tab=flow` on a DNS node — or `?tab=interfaces` on a
  // ping-only device — would paint a body whose button is not in the bar, the mirror image of the
  // ADR-031 Flow-tab bug.
  it('falls back to overview for a tab this node cannot show', () => {
    const snmpDevice: NodeDetailSubject = { kind: 'device', snmpConfigured: true };
    const pingOnly: NodeDetailSubject = { kind: 'device', snmpConfigured: false };
    expect(resolveNodeDetailTab('flow', { kind: 'dns', snmpConfigured: false })).toBe('overview');
    expect(resolveNodeDetailTab('interfaces', { kind: 'url', snmpConfigured: true })).toBe(
      'overview',
    );
    expect(resolveNodeDetailTab('events', { kind: 'url', snmpConfigured: false })).toBe('overview');
    expect(resolveNodeDetailTab('neighbors', { kind: 'dns', snmpConfigured: false })).toBe(
      'overview',
    );
    expect(resolveNodeDetailTab('flow', { kind: 'meraki', snmpConfigured: false })).toBe('overview');
    // Meraki appliances do export syslog, so Events is not hidden for them.
    expect(resolveNodeDetailTab('events', { kind: 'meraki', snmpConfigured: false })).toBe('events');
    expect(resolveNodeDetailTab('flow', snmpDevice)).toBe('flow');

    // ADR-119: the two SNMP-fed tabs bounce on a ping-only device, and the two address-attributed
    // ones do not. Both halves asserted — a rule that hid all four would satisfy only the first.
    expect(resolveNodeDetailTab('interfaces', pingOnly)).toBe('overview');
    expect(resolveNodeDetailTab('neighbors', pingOnly)).toBe('overview');
    expect(resolveNodeDetailTab('events', pingOnly)).toBe('events');
    expect(resolveNodeDetailTab('flow', pingOnly)).toBe('flow');
    expect(resolveNodeDetailTab('interfaces', snmpDevice)).toBe('interfaces');
  });

  it('rejects an unknown tab for every node', () => {
    for (const n of SUBJECTS) {
      expect(resolveNodeDetailTab('bogus', n), label(n)).toBe('overview');
      expect(resolveNodeDetailTab('', n), label(n)).toBe('overview');
      expect(resolveNodeDetailTab('FLOW', n), label(n)).toBe('overview');
    }
  });
});

const iface = (oper: number | null): InterfaceRow => ({
  ifindex: 1,
  if_name: 'eth0',
  if_alias: null,
  if_speed_bps: null,
  oper_status: oper,
  in_bps: null,
  out_bps: null,
  in_util_pct: null,
  out_util_pct: null,
  last_seen_unix: null,
  stale: false,
});

const stats = (over: Partial<NodeDetailTabStats> = {}): NodeDetailTabStats => ({
  interfaces: [],
  collCount: null,
  hasSnmp: false,
  state: 'ok',
  ...over,
});

describe('node-detail tab badges and warnings', () => {
  it('shows the interface count, but no pill when there are none', () => {
    const meta = NODE_DETAIL_TAB_META.interfaces;
    expect(meta.badge?.(stats({ interfaces: [iface(1), iface(1)] }))).toBe(2);
    expect(meta.badge?.(stats())).toBeNull();
  });

  it('warns on interfaces only when one is known-down (unknown oper_status is not a warning)', () => {
    const meta = NODE_DETAIL_TAB_META.interfaces;
    expect(meta.warn?.(stats({ interfaces: [iface(1), iface(1)] }))).toBe(false);
    expect(meta.warn?.(stats({ interfaces: [iface(1), iface(2)] }))).toBe(true);
    expect(meta.warn?.(stats({ interfaces: [iface(null)] }))).toBe(false);
  });

  it('passes the collection count through, including the loading null', () => {
    const meta = NODE_DETAIL_TAB_META.collection;
    expect(meta.badge?.(stats({ collCount: 4 }))).toBe(4);
    expect(meta.badge?.(stats({ collCount: null }))).toBeNull();
  });

  // Zero is "no pill", the same rule the interfaces badge already follows. A URL or DNS monitor's
  // built-in profile attaches no SNMP templates, so a literal `Collection 0` was reading as a
  // fault on every monitor node.
  it('shows no pill when the profile attaches no collection sets', () => {
    expect(NODE_DETAIL_TAB_META.collection.badge?.(stats({ collCount: 0 }))).toBeNull();
  });

  it('warns on collection only for an SNMP-polled node we cannot currently reach', () => {
    const meta = NODE_DETAIL_TAB_META.collection;
    expect(meta.warn?.(stats({ hasSnmp: true, state: 'unreachable' }))).toBe(true);
    expect(meta.warn?.(stats({ hasSnmp: true, state: 'unknown' }))).toBe(true);
    expect(meta.warn?.(stats({ hasSnmp: true, state: 'ok' }))).toBe(false);
    // No SNMP ⇒ nothing is expected to collect, so it is not "needs attention".
    expect(meta.warn?.(stats({ hasSnmp: false, state: 'unreachable' }))).toBe(false);
  });

  it('leaves the plain tabs undecorated', () => {
    for (const key of ['overview', 'events', 'flow'] as const) {
      expect(NODE_DETAIL_TAB_META[key].badge).toBeUndefined();
      expect(NODE_DETAIL_TAB_META[key].warn).toBeUndefined();
    }
  });
});
