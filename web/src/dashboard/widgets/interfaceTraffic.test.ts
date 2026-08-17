// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the "Interface traffic" widget's judgement (ADR-069).
//
// Every rule the widget applies lives in `interfaceTraffic.ts` precisely so it can be tested here:
// Vitest runs `environment: 'node'` and never executes `.tsx`, so anything decided in the component
// would be decided untested.
//
// Note the shape of the settings tests: they assert the ACCEPT side as well as the reject side. A
// suite that only shows malformed input being dropped passes just as well against a reader that
// drops everything, which is how a "nothing is ever selected" regression would hide.

import { describe, expect, it } from 'vitest';
// Importing the component module is safe under Vitest (nothing renders; the CSS import is a no-op)
// — the same reason `RangeControl.test.ts` gives. It is imported HERE and not by the widget's
// runtime module, which keeps a `.tsx` off the widget's import path.
import { RANGES } from '../../components/NodeDetail/RangeControl';
import type { InterfaceRow, InterfaceSeries } from '../../types/api';
import {
  DEFAULT_RANGE_SECS,
  MAX_LINKS,
  TRAFFIC_RANGES,
  availableInterfaces,
  buildTrafficSeries,
  interfaceLabel,
  interfaceTrafficPlan,
  linkId,
  linksKey,
  readTrafficSettings,
  refreshMsFor,
  selectedNodeIds,
  type LinkSeries,
  type ResolvedLink,
} from './interfaceTraffic';

const NODE_A = '11111111-1111-4111-8111-111111111111';
const NODE_B = '22222222-2222-4222-8222-222222222222';

const row = (ifindex: number, extra: Partial<InterfaceRow> = {}): InterfaceRow => ({
  ifindex,
  stale: false,
  ...extra,
});

/** An `InterfaceSeries` with every array present and the same length as `timestamps`, which is what
 *  the API guarantees — the widget's defences are about arrays that are *missing*, not ragged. */
const series = (
  timestamps: number[],
  vals: Partial<Record<keyof InterfaceSeries, (number | null)[]>> = {},
): InterfaceSeries => {
  const zeros = () => timestamps.map(() => 0);
  return {
    timestamps,
    in_bps: zeros(),
    out_bps: zeros(),
    in_ucast_pps: zeros(),
    out_ucast_pps: zeros(),
    in_errors: zeros(),
    out_errors: zeros(),
    in_discards: zeros(),
    out_discards: zeros(),
    rx_power_dbm: zeros(),
    tx_power_dbm: zeros(),
    ...vals,
  } as InterfaceSeries;
};

const resolved = (nodeId: string, ifindex: number, label: string): ResolvedLink => ({
  nodeId,
  nodeName: null,
  ifindex,
  ifName: null,
  label,
});

const PALETTE = ['c1', 'c2', 'c3', 'c4', 'c5', 'c6'];
const LABELS = { in: 'In', out: 'Out' };

describe('readTrafficSettings', () => {
  it('accepts a well-formed selection', () => {
    const sel = readTrafficSettings({
      links: [
        { nodeId: NODE_A, nodeName: 'router-a', ifindex: 7, ifName: 'GE0/0/1' },
        { nodeId: NODE_B, nodeName: 'router-b', ifindex: 12, ifName: null },
      ],
      unit: 'pps',
      rangeSecs: 86400,
    });
    expect(sel.links).toEqual([
      { nodeId: NODE_A, nodeName: 'router-a', ifindex: 7, ifName: 'GE0/0/1' },
      { nodeId: NODE_B, nodeName: 'router-b', ifindex: 12, ifName: null },
    ]);
    expect(sel.unit).toBe('pps');
    expect(sel.rangeSecs).toBe(86400);
  });

  it('defaults an absent bag to no links, bps and the shortest window', () => {
    const sel = readTrafficSettings(undefined);
    expect(sel).toEqual({ links: [], unit: 'bps', rangeSecs: DEFAULT_RANGE_SECS });
  });

  it('drops links whose ifindex is not a positive integer, and keeps the valid ones', () => {
    const sel = readTrafficSettings({
      links: [
        { nodeId: NODE_A, ifindex: '7' }, // a string that looks like a number
        { nodeId: NODE_A, ifindex: 7.5 },
        { nodeId: NODE_A, ifindex: 0 },
        { nodeId: NODE_A, ifindex: -3 },
        { nodeId: NODE_A, ifindex: Number.NaN },
        { nodeId: NODE_A, ifindex: 9 }, // the one good row
      ],
    });
    expect(sel.links).toHaveLength(1);
    expect(sel.links[0].ifindex).toBe(9);
  });

  it('drops links whose node id is missing or not a string', () => {
    const sel = readTrafficSettings({
      links: [{ nodeId: 42, ifindex: 1 }, { nodeId: '', ifindex: 2 }, { ifindex: 3 }, null, 'x'],
    });
    expect(sel.links).toEqual([]);
  });

  it('collapses duplicates of the same (node, ifindex)', () => {
    const sel = readTrafficSettings({
      links: [
        { nodeId: NODE_A, ifindex: 7 },
        { nodeId: NODE_A, ifindex: 7 },
        { nodeId: NODE_B, ifindex: 7 },
      ],
    });
    expect(sel.links.map(linkId)).toEqual([`${NODE_A}:7`, `${NODE_B}:7`]);
  });

  it('truncates a hand-edited document to the palette size', () => {
    const links = Array.from({ length: MAX_LINKS + 4 }, (_, i) => ({ nodeId: NODE_A, ifindex: i + 1 }));
    expect(readTrafficSettings({ links }).links).toHaveLength(MAX_LINKS);
  });

  it('falls back to bps and the default window for values it does not know', () => {
    const sel = readTrafficSettings({ links: [], unit: 'octets', rangeSecs: 999 });
    expect(sel.unit).toBe('bps');
    expect(sel.rangeSecs).toBe(DEFAULT_RANGE_SECS);
  });

  it('ignores a links value that is not an array', () => {
    expect(readTrafficSettings({ links: 'GE0/0/1' }).links).toEqual([]);
  });
});

describe('TRAFFIC_RANGES', () => {
  // The widget offers a deliberate subset of the app-wide windows. Pinning the relation is what
  // makes a window added to `RangeControl` a decision here rather than a silent divergence.
  it('is a subset of the shared range presets', () => {
    const shared = new Set(RANGES.map((r) => r.secs));
    for (const r of TRAFFIC_RANGES) expect(shared.has(r.secs)).toBe(true);
  });

  it('labels each window with the same text the shared control uses', () => {
    // Both lists are on screen in the same product (a widget header, a node-detail pane), so a
    // window spelled `24h` in one place and `1d` in the other would read as two different windows.
    const shared = new Map(RANGES.map((r) => [r.secs, r.label]));
    for (const r of TRAFFIC_RANGES) expect(r.label).toBe(shared.get(r.secs));
  });

  it('omits 3d, and says so by being shorter than the shared list', () => {
    expect(TRAFFIC_RANGES.map((r) => r.secs)).not.toContain(3 * 86400);
    expect(TRAFFIC_RANGES.length).toBeLessThan(RANGES.length);
  });

  it('contains the default window', () => {
    expect(TRAFFIC_RANGES.map((r) => r.secs)).toContain(DEFAULT_RANGE_SECS);
  });
});

describe('refreshMsFor', () => {
  it('gives each window its own cadence', () => {
    expect(refreshMsFor(3600)).toBe(15_000);
    expect(refreshMsFor(6 * 3600)).toBe(60_000);
    expect(refreshMsFor(24 * 3600)).toBe(300_000);
    expect(refreshMsFor(7 * 86400)).toBe(900_000);
  });

  it('never polls a wider window more often than a narrower one', () => {
    const secs = TRAFFIC_RANGES.map((r) => r.secs).sort((a, b) => a - b);
    const ms = secs.map(refreshMsFor);
    for (let i = 1; i < ms.length; i++) expect(ms[i]).toBeGreaterThanOrEqual(ms[i - 1]);
  });
});

describe('linksKey / selectedNodeIds', () => {
  it('builds a key that changes when the selection does', () => {
    const a = [{ nodeId: NODE_A, nodeName: null, ifindex: 1, ifName: null }];
    const b = [{ nodeId: NODE_A, nodeName: null, ifindex: 2, ifName: null }];
    expect(linksKey(a)).not.toBe(linksKey(b));
    // The label snapshot is not part of the identity — renaming a node must not re-arm the fetch.
    expect(linksKey([{ ...a[0], nodeName: 'renamed', ifName: 'Gi0/1' }])).toBe(linksKey(a));
  });

  it('lists each node once, in first-picked order', () => {
    const links = [
      { nodeId: NODE_B, nodeName: null, ifindex: 1, ifName: null },
      { nodeId: NODE_A, nodeName: null, ifindex: 2, ifName: null },
      { nodeId: NODE_B, nodeName: null, ifindex: 3, ifName: null },
    ];
    expect(selectedNodeIds(links)).toEqual([NODE_B, NODE_A]);
  });
});

describe('availableInterfaces', () => {
  it('hides the rows already plotted for that node but keeps the rest', () => {
    const rows = [row(1), row(2), row(3)];
    const links = [
      { nodeId: NODE_A, nodeName: null, ifindex: 2, ifName: null },
      { nodeId: NODE_B, nodeName: null, ifindex: 3, ifName: null }, // another node's pick
    ];
    expect(availableInterfaces(rows, links, NODE_A).map((r) => r.ifindex)).toEqual([1, 3]);
  });
});

describe('interfaceLabel', () => {
  it('prefers the name, then the alias, then the ifindex', () => {
    expect(interfaceLabel(7, 'GE0/0/1', 'Internet')).toBe('GE0/0/1');
    expect(interfaceLabel(7, null, 'Internet')).toBe('Internet');
    expect(interfaceLabel(7, null, null)).toBe('if7');
    expect(interfaceLabel(7, '', '')).toBe('if7');
  });
});

describe('interfaceTrafficPlan', () => {
  const sel = (links: { nodeId: string; ifindex: number; nodeName?: string }[]) => ({
    links: links.map((l) => ({
      nodeId: l.nodeId,
      nodeName: l.nodeName ?? null,
      ifindex: l.ifindex,
      ifName: null,
    })),
    unit: 'bps' as const,
    rangeSecs: 3600,
  });

  it('asks for a link when none is picked', () => {
    expect(interfaceTrafficPlan(sel([]), {})).toEqual({ kind: 'empty' });
  });

  it('waits while a roster is still loading — and does not call the link stale', () => {
    const s = sel([{ nodeId: NODE_A, ifindex: 7 }]);
    expect(interfaceTrafficPlan(s, {})).toEqual({ kind: 'loading' });
    expect(interfaceTrafficPlan(s, { [NODE_A]: null })).toEqual({ kind: 'loading' });
  });

  it('treats an empty roster as "the node reports no interfaces", not as loading', () => {
    const plan = interfaceTrafficPlan(sel([{ nodeId: NODE_A, ifindex: 7 }]), { [NODE_A]: [] });
    expect(plan).toMatchObject({ kind: 'chart', links: [] });
    expect(plan.kind === 'chart' && plan.unavailable).toEqual(['if7']);
  });

  it('draws the links that resolve and names the ones that do not', () => {
    const plan = interfaceTrafficPlan(
      sel([
        { nodeId: NODE_A, ifindex: 7, nodeName: 'router-a' },
        { nodeId: NODE_A, ifindex: 99, nodeName: 'router-a' },
      ]),
      { [NODE_A]: [row(7, { if_name: 'GE0/0/1' })] },
    );
    expect(plan.kind).toBe('chart');
    if (plan.kind !== 'chart') return;
    expect(plan.links.map((l) => l.label)).toEqual(['router-a · GE0/0/1']);
    expect(plan.unavailable).toEqual(['router-a · if99']);
  });

  it('relabels from the roster, not from the persisted snapshot', () => {
    const s = {
      links: [{ nodeId: NODE_A, nodeName: 'router-a', ifindex: 7, ifName: 'OLD-NAME' }],
      unit: 'bps' as const,
      rangeSecs: 3600,
    };
    const plan = interfaceTrafficPlan(s, { [NODE_A]: [row(7, { if_name: 'GE0/0/1' })] });
    expect(plan.kind === 'chart' && plan.links[0].label).toBe('router-a · GE0/0/1');
  });

  it('keeps drawing a link whose roster request failed, and claims nothing about it', () => {
    // A transient 500 on the roster must not blank a working chart, and must NOT be reported as
    // "this interface no longer exists" — that is a fact the failed request did not establish.
    const plan = interfaceTrafficPlan(
      {
        links: [{ nodeId: NODE_A, nodeName: 'router-a', ifindex: 7, ifName: 'GE0/0/1' }],
        unit: 'bps',
        rangeSecs: 3600,
      },
      { [NODE_A]: 'failed' },
    );
    expect(plan.kind).toBe('chart');
    if (plan.kind !== 'chart') return;
    expect(plan.links.map((l) => l.label)).toEqual(['router-a · GE0/0/1']);
    expect(plan.unavailable).toEqual([]);
  });

  it('waits for every node, not just the first', () => {
    const s = sel([
      { nodeId: NODE_A, ifindex: 7 },
      { nodeId: NODE_B, ifindex: 1 },
    ]);
    expect(interfaceTrafficPlan(s, { [NODE_A]: [row(7)] })).toEqual({ kind: 'loading' });
  });
});

describe('buildTrafficSeries', () => {
  const ts = [100, 160, 220];

  it('draws two series per link: receive positive, transmit negative', () => {
    const entries: LinkSeries[] = [
      {
        link: resolved(NODE_A, 7, 'router-a · GE0/0/1'),
        series: series(ts, { in_bps: [10, 20, 30], out_bps: [1, 2, 3] }),
      },
    ];
    const out = buildTrafficSeries(entries, 'bps', PALETTE, LABELS);
    expect(out.timestamps).toEqual(ts);
    expect(out.series).toHaveLength(2);
    expect(out.series[0]).toMatchObject({ label: 'router-a · GE0/0/1 In', values: [10, 20, 30] });
    expect(out.series[1]).toMatchObject({ label: 'router-a · GE0/0/1 Out', values: [-1, -2, -3] });
  });

  it('gives both directions of one link the same colour, and each link a different one', () => {
    const entries: LinkSeries[] = [
      { link: resolved(NODE_A, 7, 'a'), series: series(ts) },
      { link: resolved(NODE_B, 1, 'b'), series: series(ts) },
    ];
    const colors = buildTrafficSeries(entries, 'bps', PALETTE, LABELS).series.map((s) => s.color);
    expect(colors).toEqual(['c1', 'c1', 'c2', 'c2']);
  });

  it('keeps a gap as a gap — a missing sample is a hole, not zero traffic', () => {
    const entries: LinkSeries[] = [
      {
        link: resolved(NODE_A, 7, 'a'),
        series: series(ts, { in_bps: [10, null, 30], out_bps: [null, 2, null] }),
      },
    ];
    const out = buildTrafficSeries(entries, 'bps', PALETTE, LABELS);
    expect(out.series[0].values).toEqual([10, null, 30]);
    expect(out.series[1].values).toEqual([null, -2, null]);
  });

  it('reads a different pair of arrays for pps than for bps', () => {
    // The four candidate arrays share a type, so a swapped pair compiles and renders a pps axis
    // drawn from bps values. This is the check that the unit actually reaches the array choice.
    const entries: LinkSeries[] = [
      {
        link: resolved(NODE_A, 7, 'a'),
        series: series(ts, {
          in_bps: [8000, 8000, 8000],
          out_bps: [4000, 4000, 4000],
          in_ucast_pps: [10, 10, 10],
          out_ucast_pps: [5, 5, 5],
        }),
      },
    ];
    const bps = buildTrafficSeries(entries, 'bps', PALETTE, LABELS);
    const pps = buildTrafficSeries(entries, 'pps', PALETTE, LABELS);
    expect(bps.series[0].values).toEqual([8000, 8000, 8000]);
    expect(bps.series[1].values).toEqual([-4000, -4000, -4000]);
    expect(pps.series[0].values).toEqual([10, 10, 10]);
    expect(pps.series[1].values).toEqual([-5, -5, -5]);
  });

  it('places values by timestamp when a link answers on a different axis', () => {
    const entries: LinkSeries[] = [
      { link: resolved(NODE_A, 7, 'a'), series: series(ts, { in_bps: [1, 2, 3] }) },
      // Second link is missing the first sample and carries an extra one past the axis.
      {
        link: resolved(NODE_B, 1, 'b'),
        series: series([160, 220, 280], { in_bps: [20, 30, 40] }),
      },
    ];
    const out = buildTrafficSeries(entries, 'bps', PALETTE, LABELS);
    expect(out.timestamps).toEqual(ts);
    // 100 has no sample on link b, 160/220 line up, and 280 falls outside the axis entirely.
    expect(out.series[2].values).toEqual([null, 20, 30]);
  });

  it('drops a link whose fetch failed without moving the others onto its colour', () => {
    const entries: LinkSeries[] = [
      { link: resolved(NODE_A, 7, 'a'), series: null },
      { link: resolved(NODE_B, 1, 'b'), series: series(ts, { in_bps: [1, 2, 3] }) },
    ];
    const out = buildTrafficSeries(entries, 'bps', PALETTE, LABELS);
    expect(out.series).toHaveLength(2);
    expect(out.series.map((s) => s.color)).toEqual(['c2', 'c2']);
    expect(out.timestamps).toEqual(ts);
  });

  it('returns nothing to draw when no link answered', () => {
    const entries: LinkSeries[] = [
      { link: resolved(NODE_A, 7, 'a'), series: null },
      { link: resolved(NODE_B, 1, 'b'), series: series([]) },
    ];
    expect(buildTrafficSeries(entries, 'bps', PALETTE, LABELS)).toEqual({
      timestamps: [],
      series: [],
    });
  });

  it('survives a core that predates the pps fields', () => {
    // web and core are separate containers, so a new WebUI can talk to an older core. The generated
    // type says the arrays are always present; that is a statement about *this* core.
    const legacy = { timestamps: ts, in_bps: [1, 2, 3], out_bps: [1, 2, 3] } as unknown as InterfaceSeries;
    const out = buildTrafficSeries(
      [{ link: resolved(NODE_A, 7, 'a'), series: legacy }],
      'pps',
      PALETTE,
      LABELS,
    );
    expect(out.series[0].values).toEqual([null, null, null]);
  });
});
