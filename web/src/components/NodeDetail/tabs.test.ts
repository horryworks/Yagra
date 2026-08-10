// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  NODE_DETAIL_TAB_META,
  NODE_DETAIL_TABS,
  normalizeNodeDetailTab,
  resolveNodeDetailTab,
  visibleNodeDetailTabs,
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

// Which kinds see which tabs. A URL or DNS monitor produces one HTTP/DNS job and nothing else
// (scheduler.rs::assemble_node_jobs returns before the ICMP and SNMP branches), and a Meraki node
// emits no per-node job at all — so Interfaces / Neighbors / Flow are structurally unreachable for
// them, not merely empty today.
describe('node-detail tab visibility by kind', () => {
  it('gives every whitelisted tab a non-empty list of real node kinds', () => {
    for (const tab of NODE_DETAIL_TABS) {
      const kinds = NODE_DETAIL_TAB_META[tab].kinds;
      expect(kinds.length, tab).toBeGreaterThan(0);
      for (const k of kinds) expect(NODE_KINDS, tab).toContain(k);
      expect(new Set(kinds).size, tab).toBe(kinds.length);
    }
  });

  // Load-bearing: 'overview' being visible everywhere is what makes resolveNodeDetailTab's
  // fallback terminate, and what makes NodesPage's "a fresh selection starts on Overview" path
  // (it deletes the ?tab= param) valid for every kind. Do not narrow it.
  it('shows every kind at least the overview tab', () => {
    for (const kind of NODE_KINDS) {
      expect(visibleNodeDetailTabs(kind), kind).toContain('overview');
    }
  });

  // An orphan tab would be a dead body element and a dead locale key that i18n parity still passes.
  it('shows every tab to at least one kind', () => {
    const shown = new Set(NODE_KINDS.flatMap((k) => visibleNodeDetailTabs(k)));
    expect([...shown].sort()).toEqual([...NODE_DETAIL_TABS].sort());
  });

  it('pins the kind → tab matrix', () => {
    expect([...visibleNodeDetailTabs('device')]).toEqual([...NODE_DETAIL_TABS]);
    expect([...visibleNodeDetailTabs('url')]).toEqual(['overview', 'collection']);
    expect([...visibleNodeDetailTabs('dns')]).toEqual(['overview', 'collection']);
    expect([...visibleNodeDetailTabs('meraki')]).toEqual(['overview', 'collection', 'events']);
  });

  // The bar's order comes from NODE_DETAIL_TABS, never from a per-kind list, so no kind can
  // reshuffle the tabs relative to another.
  it('keeps the registry order for every kind', () => {
    for (const kind of NODE_KINDS) {
      const visible = visibleNodeDetailTabs(kind);
      const positions = visible.map((tab) => NODE_DETAIL_TABS.indexOf(tab));
      expect(positions, kind).toEqual([...positions].sort((a, b) => a - b));
    }
  });
});

describe('resolveNodeDetailTab', () => {
  it('is kind-blind while the kind is still unknown', () => {
    for (const tab of NODE_DETAIL_TABS) expect(resolveNodeDetailTab(tab, null)).toBe(tab);
    expect(resolveNodeDetailTab('bogus', null)).toBe('overview');
  });

  it('resolves every tab a kind does show to itself', () => {
    for (const kind of NODE_KINDS) {
      for (const tab of visibleNodeDetailTabs(kind)) {
        expect(resolveNodeDetailTab(tab, kind), `${kind}/${tab}`).toBe(tab);
      }
    }
  });

  // The bookmarked-URL case: without this, `?tab=flow` on a DNS node would paint a body whose
  // button is not in the bar — the mirror image of the ADR-031 Flow-tab bug.
  it('falls back to overview for a tab this kind cannot show', () => {
    expect(resolveNodeDetailTab('flow', 'dns')).toBe('overview');
    expect(resolveNodeDetailTab('interfaces', 'url')).toBe('overview');
    expect(resolveNodeDetailTab('events', 'url')).toBe('overview');
    expect(resolveNodeDetailTab('neighbors', 'dns')).toBe('overview');
    expect(resolveNodeDetailTab('flow', 'meraki')).toBe('overview');
    // Meraki appliances do export syslog, so Events is not hidden for them.
    expect(resolveNodeDetailTab('events', 'meraki')).toBe('events');
    expect(resolveNodeDetailTab('flow', 'device')).toBe('flow');
  });

  it('rejects an unknown tab for every kind', () => {
    for (const kind of NODE_KINDS) {
      expect(resolveNodeDetailTab('bogus', kind), kind).toBe('overview');
      expect(resolveNodeDetailTab('', kind), kind).toBe('overview');
      expect(resolveNodeDetailTab('FLOW', kind), kind).toBe('overview');
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
  hasCredential: false,
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

  it('warns on collection only for an SNMP-bound node we cannot currently reach', () => {
    const meta = NODE_DETAIL_TAB_META.collection;
    expect(meta.warn?.(stats({ hasCredential: true, state: 'unreachable' }))).toBe(true);
    expect(meta.warn?.(stats({ hasCredential: true, state: 'unknown' }))).toBe(true);
    expect(meta.warn?.(stats({ hasCredential: true, state: 'ok' }))).toBe(false);
    // No credential bound ⇒ nothing is expected to collect, so it is not "needs attention".
    expect(meta.warn?.(stats({ hasCredential: false, state: 'unreachable' }))).toBe(false);
  });

  it('leaves the plain tabs undecorated', () => {
    for (const key of ['overview', 'events', 'flow'] as const) {
      expect(NODE_DETAIL_TAB_META[key].badge).toBeUndefined();
      expect(NODE_DETAIL_TAB_META[key].warn).toBeUndefined();
    }
  });
});
