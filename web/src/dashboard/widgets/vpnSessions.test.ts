// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the "VPN sessions" widget's judgement (ADR-136).
//
// Every rule the widget applies lives in `vpnSessions.ts` precisely so it can be tested here:
// Vitest runs `environment: 'node'` and never executes `.tsx`, so anything decided in the component
// would be decided untested.
//
// Note the shape of the settings tests: they assert the ACCEPT side as well as the reject side. A
// suite that only shows malformed input being dropped passes just as well against a reader that
// drops everything, which is how a "nothing is ever selected" regression would hide
// (`rejection-only-tests-pass-when-everything-rejects`).

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { metricUnit } from '../../lib/format';
import type { MetricRange, NodeMetricEntry } from '../../types/api';
import { DEFAULT_WIDGET_RANGE_SECS } from './util';
import {
  MAX_VPN_NODES,
  VPN_SESSION_METRICS,
  armedKey,
  buildVpnSeries,
  currentReadings,
  everyNodeFailed,
  latestOf,
  nodesKey,
  readVpnSettings,
  resolveVpnMetric,
  vpnSessionsPlan,
  type ResolvedVpnNode,
  type VpnInventory,
  type VpnNodeSeries,
} from './vpnSessions';

const NODE_A = '11111111-1111-4111-8111-111111111111';
const NODE_B = '22222222-2222-4222-8222-222222222222';
const NODE_C = '33333333-3333-4333-8333-333333333333';

const PALETTE = ['c1', 'c2', 'c3', 'c4', 'c5', 'c6'];

const entry = (metric: string, extra: Partial<NodeMetricEntry> = {}): NodeMetricEntry => ({
  metric,
  metric_kind: 'gauge',
  dimension: 'none',
  series_count: 1,
  status: 'ok',
  ...extra,
});

const range = (nodeId: string, points: [number, number][]): MetricRange => ({
  metric: 'cisco_ra_sessions',
  node_id: nodeId,
  points: points.map(([t, v]) => ({ t, v })),
});

const resolved = (nodeId: string, label: string, metric = 'cisco_ra_sessions'): ResolvedVpnNode => ({
  nodeId,
  nodeName: label,
  label,
  metric,
  query: {},
});

describe('VPN_SESSION_METRICS', () => {
  // ADR-136 決定 3. The three values do not mean the same thing — sessions, users, tunnels — and the
  // only reason mixing them in one chart is honest is that each number is rendered with its own
  // noun. A fourth entry added with no row in the generated unit table would render as a bare
  // number, which is the state this ladder is not allowed to be in.
  it('gives every candidate a unit, so the mixing is visible on screen', () => {
    for (const m of VPN_SESSION_METRICS) {
      expect(metricUnit(m), `${m} has no unit`).not.toBeNull();
    }
  });

  it('reads each candidate as a counted noun rather than a symbol', () => {
    // `counted` is what gets localized through `format:unit.*`; a symbol would go through verbatim
    // and read as English on a Japanese screen.
    for (const m of VPN_SESSION_METRICS) {
      expect(metricUnit(m)?.kind, m).toBe('counted');
    }
  });

  it('leaves the per-user count out', () => {
    // One person with a laptop and a phone is one user and two sessions; the device's load is the
    // second number, and the user chose it. Pinned so a later "it says users on FortiGate, let us
    // add the Cisco one too" does not quietly change what the widget means.
    expect([...VPN_SESSION_METRICS]).not.toContain('cisco_ra_users');
  });

  it('names each candidate once', () => {
    expect(new Set(VPN_SESSION_METRICS).size).toBe(VPN_SESSION_METRICS.length);
  });
});

describe('MAX_VPN_NODES', () => {
  // The cap exists because a seventh line would wrap to the first palette colour and the legend
  // would name a colour the line does not use — a failure that looks like a working chart. Reading
  // the palette's real length is what turns that from a comment into something that breaks when
  // someone adds a seventh series colour.
  it('is exactly the chart palette length', () => {
    const src = readFileSync(
      join(__dirname, '..', '..', 'components', 'MetricChart', 'MetricChart.tsx'),
      'utf8',
    );
    const start = src.indexOf('export const PALETTE = [');
    expect(start, 'PALETTE declaration not found — this reader has stopped matching').toBeGreaterThan(-1);
    const block = src.slice(start, src.indexOf('];', start));
    const colors = [...block.matchAll(/var\(--series-\d+\)/g)].length;
    // The floor counts what was actually inspected: a regex that stopped matching would otherwise
    // report zero and be indistinguishable from a palette that shrank (`floor-must-count-what-was-checked`).
    expect(colors).toBeGreaterThanOrEqual(3);
    expect(MAX_VPN_NODES).toBe(colors);
  });
});

describe('readVpnSettings', () => {
  it('reads a well-formed selection back unchanged', () => {
    const sel = readVpnSettings({
      nodes: [
        { nodeId: NODE_A, nodeName: 'asa-tokyo' },
        { nodeId: NODE_B, nodeName: 'fw-osaka' },
      ],
      rangeSecs: 6 * 3600,
    });
    expect(sel).toEqual({
      nodes: [
        { nodeId: NODE_A, nodeName: 'asa-tokyo' },
        { nodeId: NODE_B, nodeName: 'fw-osaka' },
      ],
      rangeSecs: 6 * 3600,
    });
  });

  it('defaults an empty bag to no nodes and the default window', () => {
    expect(readVpnSettings(undefined)).toEqual({ nodes: [], rangeSecs: DEFAULT_WIDGET_RANGE_SECS });
    expect(readVpnSettings({})).toEqual({ nodes: [], rangeSecs: DEFAULT_WIDGET_RANGE_SECS });
  });

  it('drops entries that are not objects with a string node id', () => {
    const sel = readVpnSettings({
      nodes: [
        'asa-tokyo',
        null,
        42,
        { nodeName: 'no id at all' },
        { nodeId: 7 },
        { nodeId: '' },
        { nodeId: NODE_A, nodeName: 'asa-tokyo' },
      ],
    });
    expect(sel.nodes).toEqual([{ nodeId: NODE_A, nodeName: 'asa-tokyo' }]);
  });

  it('keeps a node whose name is missing, because the id is the identity', () => {
    // The name is a label snapshot; losing it must not lose the selection.
    expect(readVpnSettings({ nodes: [{ nodeId: NODE_A }] }).nodes).toEqual([
      { nodeId: NODE_A, nodeName: null },
    ]);
  });

  it('collapses a node picked twice', () => {
    const sel = readVpnSettings({
      nodes: [
        { nodeId: NODE_A, nodeName: 'first' },
        { nodeId: NODE_A, nodeName: 'second' },
      ],
    });
    expect(sel.nodes).toEqual([{ nodeId: NODE_A, nodeName: 'first' }]);
  });

  it('truncates a hand-edited document to the palette', () => {
    const nodes = Array.from({ length: MAX_VPN_NODES + 4 }, (_, i) => ({ nodeId: `n${i}` }));
    expect(readVpnSettings({ nodes }).nodes).toHaveLength(MAX_VPN_NODES);
  });

  it('falls back to the default window for a value no range offers', () => {
    expect(readVpnSettings({ rangeSecs: 999 }).rangeSecs).toBe(DEFAULT_WIDGET_RANGE_SECS);
    expect(readVpnSettings({ rangeSecs: '3600' }).rangeSecs).toBe(DEFAULT_WIDGET_RANGE_SECS);
  });
});

describe('nodesKey', () => {
  it('changes when the selection does', () => {
    const a = [{ nodeId: NODE_A, nodeName: null }];
    const b = [{ nodeId: NODE_B, nodeName: null }];
    expect(nodesKey(a)).not.toBe(nodesKey(b));
    expect(nodesKey(a)).toBe(nodesKey([{ nodeId: NODE_A, nodeName: 'renamed' }]));
  });

  it('is empty for an empty selection', () => {
    expect(nodesKey([])).toBe('');
  });
});

describe('armedKey', () => {
  it('changes when a node resolves onto a different metric', () => {
    // The failure this exists for: an operator edits a collection set, the node now reports a
    // higher-priority candidate, and a key built from ids alone would keep polling the old name
    // until the widget happened to remount.
    const cisco = [resolved(NODE_A, 'fw', 'cisco_ra_sessions')];
    const forti = [resolved(NODE_A, 'fw', 'fortinet_sslvpn_users')];
    expect(nodesKey(cisco)).toBe(nodesKey(forti));
    expect(armedKey(cisco)).not.toBe(armedKey(forti));
  });

  it('is empty when nothing resolved', () => {
    expect(armedKey([])).toBe('');
  });
});

describe('resolveVpnMetric', () => {
  it('takes the Cisco session count on an ASA', () => {
    const found = resolveVpnMetric([entry('icmp_rtt_ms'), entry('cisco_ra_sessions')]);
    expect(found?.metric).toBe('cisco_ra_sessions');
  });

  it('takes the FortiGate count when that is what the device has', () => {
    // Per-VDOM table: `entity` dimension, so the query collapses with agg=max rather than a plain
    // range. It is still a candidate.
    const found = resolveVpnMetric([entry('fortinet_sslvpn_users', { dimension: 'entity' })]);
    expect(found?.metric).toBe('fortinet_sslvpn_users');
  });

  it('follows the ladder order, not the inventory order', () => {
    const found = resolveVpnMetric([
      entry('panos_gp_active_tunnels'),
      entry('fortinet_sslvpn_users'),
      entry('cisco_ra_sessions'),
    ]);
    expect(found?.metric).toBe('cisco_ra_sessions');
  });

  it('returns null when the device reports none of them', () => {
    expect(resolveVpnMetric([entry('icmp_rtt_ms'), entry('if_hc_in_octets')])).toBeNull();
    expect(resolveVpnMetric([])).toBeNull();
  });

  it('takes a candidate that has produced no data yet', () => {
    // Configured and silent is a real answer: an empty chart the operator can leave on the board
    // while they fix the device. Falling through to a lower-priority candidate would answer a
    // different vendor's question without saying so.
    const found = resolveVpnMetric([entry('cisco_ra_sessions', { status: 'no_data' })]);
    expect(found?.metric).toBe('cisco_ra_sessions');
  });

  it('refuses a candidate the decision table will not chart', () => {
    // A per-entity counter has no node-level rate and no query at all; offering it would produce a
    // card with an empty chart under it and no explanation.
    expect(
      resolveVpnMetric([
        entry('fortinet_sslvpn_users', { dimension: 'entity', metric_kind: 'counter' }),
      ]),
    ).toBeNull();
    // Per-interface belongs to the Interfaces tab, never to this card.
    expect(
      resolveVpnMetric([entry('cisco_ra_sessions', { dimension: 'interface' })]),
    ).toBeNull();
  });
});

describe('vpnSessionsPlan', () => {
  const sel = (...nodes: [string, string][]) => ({
    nodes: nodes.map(([nodeId, nodeName]) => ({ nodeId, nodeName })),
    rangeSecs: 3600,
  });

  it('says nothing is picked', () => {
    expect(vpnSessionsPlan({ nodes: [], rangeSecs: 3600 }, {})).toEqual({ kind: 'empty' });
  });

  it('waits while any inventory is still loading', () => {
    const s = sel([NODE_A, 'asa-tokyo'], [NODE_B, 'fw-osaka']);
    // Absent entirely.
    expect(vpnSessionsPlan(s, { [NODE_A]: [entry('cisco_ra_sessions')] }).kind).toBe('loading');
    // Explicitly null.
    const inv: Record<string, VpnInventory> = {
      [NODE_A]: [entry('cisco_ra_sessions')],
      [NODE_B]: null,
    };
    expect(vpnSessionsPlan(s, inv).kind).toBe('loading');
  });

  it('does not treat an empty inventory as still loading', () => {
    // `[]` is a real answer — the device reports nothing — and rendering it as "loading" would spin
    // forever on a node that will never have a VPN metric.
    const plan = vpnSessionsPlan(sel([NODE_A, 'switch-1']), { [NODE_A]: [] });
    expect(plan).toEqual({ kind: 'chart', nodes: [], unsupported: ['switch-1'], unreadable: [] });
  });

  it('separates "reports none" from "could not be read"', () => {
    const plan = vpnSessionsPlan(
      sel([NODE_A, 'asa-tokyo'], [NODE_B, 'switch-1'], [NODE_C, 'fw-nagoya']),
      {
        [NODE_A]: [entry('cisco_ra_sessions')],
        [NODE_B]: [entry('icmp_rtt_ms')],
        [NODE_C]: 'failed',
      },
    );
    expect(plan.kind).toBe('chart');
    if (plan.kind !== 'chart') return;
    expect(plan.nodes.map((n) => n.label)).toEqual(['asa-tokyo']);
    expect(plan.unsupported).toEqual(['switch-1']);
    expect(plan.unreadable).toEqual(['fw-nagoya']);
  });

  it('carries the query shape the decision table chose', () => {
    const plan = vpnSessionsPlan(sel([NODE_A, 'asa'], [NODE_B, 'fgt']), {
      [NODE_A]: [entry('cisco_ra_sessions')],
      [NODE_B]: [entry('fortinet_sslvpn_users', { dimension: 'entity' })],
    });
    if (plan.kind !== 'chart') throw new Error('expected a chart');
    expect(plan.nodes[0].query).toEqual({});
    // A per-entity gauge collapses to one node series; picking this here rather than in the
    // component is what stops the widget inventing a reading (ADR-046 Inc.6 決定 L).
    expect(plan.nodes[1].query).toEqual({ agg: 'max' });
  });

  it('falls back to the node id when the saved name is gone', () => {
    const plan = vpnSessionsPlan(
      { nodes: [{ nodeId: NODE_A, nodeName: null }], rangeSecs: 3600 },
      { [NODE_A]: [entry('cisco_ra_sessions')] },
    );
    if (plan.kind !== 'chart') throw new Error('expected a chart');
    expect(plan.nodes[0].label).toBe(NODE_A);
  });
});

describe('buildVpnSeries', () => {
  it('draws one line per device that answered', () => {
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'asa-tokyo'), range: range(NODE_A, [[10, 5], [20, 7]]) },
      { node: resolved(NODE_B, 'fw-osaka'), range: range(NODE_B, [[10, 1], [20, 2]]) },
    ];
    const out = buildVpnSeries(entries, PALETTE);
    expect(out.timestamps).toEqual([10, 20]);
    expect(out.series.map((s) => s.label)).toEqual(['asa-tokyo', 'fw-osaka']);
    expect(out.series[0].values).toEqual([5, 7]);
    expect(out.series[1].values).toEqual([1, 2]);
  });

  it('places values by timestamp, not by array position', () => {
    // The caller asks every node for the same window, so the axes normally agree — but a mismatch
    // would shift one device's history against the others while still drawing a plausible chart.
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'a'), range: range(NODE_A, [[10, 1], [20, 2], [30, 3]]) },
      { node: resolved(NODE_B, 'b'), range: range(NODE_B, [[20, 9], [30, 8]]) },
    ];
    const out = buildVpnSeries(entries, PALETTE);
    expect(out.timestamps).toEqual([10, 20, 30]);
    expect(out.series[1].values).toEqual([null, 9, 8]);
  });

  it('leaves a missing sample as a gap rather than a zero', () => {
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'a'), range: range(NODE_A, [[10, 4], [20, 4], [30, 4]]) },
      { node: resolved(NODE_B, 'b'), range: range(NODE_B, [[10, 6], [30, 6]]) },
    ];
    const out = buildVpnSeries(entries, PALETTE);
    // A zero here would claim the concentrator had nobody connected at a moment nothing measured.
    expect(out.series[1].values).toEqual([6, null, 6]);
  });

  it('colours by position in the selection, so a failed device does not shift the others', () => {
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'a'), range: null },
      { node: resolved(NODE_B, 'b'), range: range(NODE_B, [[10, 1]]) },
    ];
    const out = buildVpnSeries(entries, PALETTE);
    expect(out.series).toHaveLength(1);
    // `c2`, not `c1`: the surviving line keeps the colour it had while both were drawn.
    expect(out.series[0].color).toBe('c2');
  });

  it('returns nothing to draw when no device answered with points', () => {
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'a'), range: null },
      { node: resolved(NODE_B, 'b'), range: range(NODE_B, []) },
    ];
    expect(buildVpnSeries(entries, PALETTE)).toEqual({ timestamps: [], series: [] });
    expect(buildVpnSeries([], PALETTE)).toEqual({ timestamps: [], series: [] });
  });
});

describe('everyNodeFailed', () => {
  it('tells a total failure from a quiet concentrator', () => {
    const failed: VpnNodeSeries[] = [{ node: resolved(NODE_A, 'a'), range: null }];
    const quiet: VpnNodeSeries[] = [{ node: resolved(NODE_A, 'a'), range: range(NODE_A, []) }];
    expect(everyNodeFailed(failed)).toBe(true);
    expect(everyNodeFailed(quiet)).toBe(false);
  });

  it('is false for a mixed result — one device answering is not a failure', () => {
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'a'), range: null },
      { node: resolved(NODE_B, 'b'), range: range(NODE_B, [[10, 1]]) },
    ];
    expect(everyNodeFailed(entries)).toBe(false);
  });

  it('is false when nothing was asked', () => {
    expect(everyNodeFailed([])).toBe(false);
  });
});

describe('currentReadings', () => {
  it('gives every selected device a row, including one that answered with nothing', () => {
    // A device that silently vanished from a strip of six is indistinguishable from one nobody
    // selected.
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'asa-tokyo'), range: range(NODE_A, [[10, 5], [20, 148]]) },
      { node: resolved(NODE_B, 'fw-osaka', 'fortinet_sslvpn_users'), range: null },
    ];
    expect(currentReadings(entries, PALETTE)).toEqual([
      { nodeId: NODE_A, label: 'asa-tokyo', metric: 'cisco_ra_sessions', value: 148, color: 'c1' },
      {
        nodeId: NODE_B,
        label: 'fw-osaka',
        metric: 'fortinet_sslvpn_users',
        value: null,
        color: 'c2',
      },
    ]);
  });

  it("carries the colour of that device's own line", () => {
    const entries: VpnNodeSeries[] = [
      { node: resolved(NODE_A, 'a'), range: range(NODE_A, [[10, 1]]) },
      { node: resolved(NODE_B, 'b'), range: range(NODE_B, [[10, 2]]) },
    ];
    const readings = currentReadings(entries, PALETTE);
    const series = buildVpnSeries(entries, PALETTE).series;
    expect(readings.map((r) => r.color)).toEqual(series.map((s) => s.color));
  });
});

describe('latestOf', () => {
  it('takes the most recent sample', () => {
    expect(latestOf(range(NODE_A, [[10, 1], [20, 9]]))).toBe(9);
  });

  it('skips a trailing non-finite value rather than reporting it', () => {
    expect(latestOf(range(NODE_A, [[10, 4], [20, Number.NaN]]))).toBe(4);
  });

  it('is null for no series and for an empty one', () => {
    expect(latestOf(null)).toBeNull();
    expect(latestOf(range(NODE_A, []))).toBeNull();
  });
});
