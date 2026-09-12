// SPDX-License-Identifier: AGPL-3.0-only
// What the "VPN sessions" widget should do, given its persisted selection and the metric
// inventories of the nodes it names (ADR-136).
//
// The collection side of this widget already existed and needed nothing: the built-in
// `Cisco remote-access VPN` template is attached to the `Cisco ASA firewall` and
// `Cisco Firepower (FTD)` profiles, and `classification.rs` binds an ASA to that profile from its
// sysDescr — so a registered ASA has been reporting `cisco_ra_sessions` all along. What was missing
// was a place to put it, and the thing a generic `metric-chart` cannot do: **know which name to
// read**, when the name differs per vendor and the operator should not have to.
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`, so
// judgement left in the `.tsx` is judgement nothing tests.

import type { ChartSeries } from '../../components/MetricChart/MetricChart';
import { metricView, rangeOptsFor, type MetricRangeOpts } from '../../lib/metricInventory';
import type { MetricRange, NodeMetricEntry } from '../../types/api';
import type { WidgetSettings } from '../types';
import { DEFAULT_WIDGET_RANGE_SECS, WIDGET_RANGES } from './util';

/**
 * How many VPN heads one widget may plot.
 *
 * This is `MetricChart.PALETTE.length`, and the coupling is the point: a seventh node would wrap to
 * the first colour, and a legend naming a colour the line does not use is a failure that *looks like
 * a working chart* — the trap ADR-046 Inc.5 pinned a test against, and the reason
 * `interfaceTraffic.ts::MAX_LINKS` is the same number. Capping the feature at the palette makes that
 * failure unreachable rather than merely unlikely.
 */
export const MAX_VPN_NODES = 6;

/**
 * The metrics that answer "how many remote-access VPN connections does this device have", in the
 * order they are tried. The first one a node's inventory carries wins.
 *
 * 🚨 **Only the first one is literally a session count, and that is on the screen rather than in
 * this comment.** `crasNumSessions` is sessions; FortiGate's is SSL-VPN *users*; Palo Alto's is
 * GlobalProtect *tunnels*. They are each the nearest thing that vendor's MIB exposes, and mixing
 * them in one chart is only honest because ADR-046 Inc.7's unit table gives every one of them a
 * noun — so a reading renders as `148 sessions` beside `74 users` and the operator can see the
 * difference rather than being told about it in prose they will never read (ADR-136 決定 3).
 * `everyLadderMetricHasAUnit` pins that: a fourth entry added here with no row in the unit table
 * would silently render a bare number, which is the state this ladder is not allowed to be in.
 *
 * ⚠️ `cisco_ra_users` (`crasNumUsers`) is deliberately absent. One person with a laptop and a phone
 * is one user and two sessions, and the device's load is the second number.
 *
 * ⚠️ The same ladder shape as `metricCards.ts::METRIC_CARDS` — priority order, first hit wins — for
 * the same reason: the vendors disagree about the name, not about the question.
 */
export const VPN_SESSION_METRICS = [
  'cisco_ra_sessions',
  'fortinet_sslvpn_users',
  'panos_gp_active_tunnels',
] as const;

/** One picked VPN head. `nodeName` is a snapshot taken when it was picked, used as a provisional
 *  label and in the "this device reports none" sentence — never as the identity, which is `nodeId`
 *  alone. */
export interface VpnNodeRef {
  nodeId: string;
  nodeName: string | null;
}

/** The widget's persisted selection. */
export interface VpnSelection {
  nodes: VpnNodeRef[];
  rangeSecs: number;
}

const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);

/**
 * Read a selection out of the opaque settings bag, dropping anything malformed.
 *
 * The bag is `Record<string, unknown>`: user-editable JSON that has round-tripped through
 * localStorage and the server. A number where a node id belongs must degrade to "that node was
 * never picked", not to a request for `/nodes/42/metrics`. Duplicates are collapsed and the list is
 * truncated to {@link MAX_VPN_NODES}, so a hand-edited document cannot exceed the palette.
 */
export function readVpnSettings(settings: WidgetSettings | undefined): VpnSelection {
  const raw = Array.isArray(settings?.nodes) ? settings.nodes : [];
  const nodes: VpnNodeRef[] = [];
  const seen = new Set<string>();
  for (const item of raw) {
    if (typeof item !== 'object' || item === null) continue;
    const rec = item as Record<string, unknown>;
    const nodeId = str(rec.nodeId);
    if (!nodeId || seen.has(nodeId)) continue;
    seen.add(nodeId);
    nodes.push({ nodeId, nodeName: str(rec.nodeName) });
    if (nodes.length >= MAX_VPN_NODES) break;
  }
  const asked = settings?.rangeSecs;
  const rangeSecs =
    typeof asked === 'number' && WIDGET_RANGES.some((r) => r.secs === asked)
      ? asked
      : DEFAULT_WIDGET_RANGE_SECS;
  return { nodes, rangeSecs };
}

/** A stable dependency key for the whole selection. Passing the array itself to `usePolled` would
 *  re-arm the fetch on every render, because a fresh array is parsed out of the settings bag each
 *  time. */
export function nodesKey(nodes: readonly VpnNodeRef[]): string {
  return nodes.map((n) => n.nodeId).join(',');
}

/**
 * Which of this node's metrics answers the question, or `null` when none of them does.
 *
 * Walks {@link VPN_SESSION_METRICS} in order and takes the first the inventory carries **and**
 * `metricView` is willing to chart. The second half is not belt-and-braces: the query shape is not
 * free choice here (ADR-046 Inc.6 決定 L), and a candidate the table refuses is one this widget has
 * no honest way to draw — offering it would produce a card with an empty chart under it and no
 * explanation.
 *
 * ⚠️ A metric whose `status` is `no_data` **is** taken. It was configured and has produced nothing,
 * and an empty chart the operator can leave on the board while they fix the device is the honest
 * rendering of that — the same call `chartableMetrics` makes for the metric-chart widget. Skipping
 * it would fall through to a lower-priority candidate on a device that has both, which is worse: it
 * would answer a different vendor's question without saying so.
 */
export function resolveVpnMetric(entries: readonly NodeMetricEntry[]): NodeMetricEntry | null {
  for (const name of VPN_SESSION_METRICS) {
    const entry = entries.find((e) => e.metric === name);
    if (!entry) continue;
    const chart = metricView(entry.metric_kind, entry.dimension).chart;
    if (chart.kind === 'range' || chart.kind === 'rate' || chart.kind === 'aggregate') return entry;
  }
  return null;
}

/**
 * One node's metric inventory, as the widget knows it.
 *
 * Three states, and collapsing any pair of them tells the operator something untrue:
 *  - `null` — still loading. Not "this device has no VPN": conflating it with `[]` would flash
 *    "reports none" every time a node is added.
 *  - `NodeMetricEntry[]` — the node's current inventory. An empty array is a real answer.
 *  - `'failed'` — the inventory request itself failed. **Not the same as "reports none"**: a
 *    transient 500 must not be rendered as a statement about what the device does.
 */
export type VpnInventory = NodeMetricEntry[] | null | 'failed';

/** A node that resolved against its current inventory: which metric to read, and how. */
export interface ResolvedVpnNode extends VpnNodeRef {
  /** The chart series' label. The node's name — the metric is carried by the reading's unit. */
  label: string;
  metric: string;
  query: MetricRangeOpts;
}

/** What the widget body should render this pass. */
export type VpnPlan =
  /** Nothing picked yet. */
  | { kind: 'empty' }
  /** Nodes are picked but at least one inventory has not arrived, so no label is trustworthy. */
  | { kind: 'loading' }
  /**
   * Draw these. `unsupported` names the picked nodes that report none of the ladder's metrics;
   * `unreadable` names the ones whose inventory could not be fetched at all.
   */
  | { kind: 'chart'; nodes: ResolvedVpnNode[]; unsupported: string[]; unreadable: string[] };

/** The label a node falls back to before — or instead of — resolving. */
function nameOf(n: VpnNodeRef): string {
  return n.nodeName ?? n.nodeId;
}

/**
 * Decide what to render.
 *
 * `inventory[nodeId] === null` (or absent) means the list is still loading — deliberately distinct
 * from `[]`, which means the node genuinely reports no metrics.
 *
 * ⚠️ **`'failed'` is named, not drawn**, and this is where the widget differs from Interface
 * traffic. That one keeps plotting a link whose roster request failed, because `(nodeId, ifindex)`
 * is the whole identity of what it is asking for and a saved label is enough to carry it. Here the
 * identity is incomplete: *which metric to read* is only knowable from the inventory, so continuing
 * would mean guessing a vendor. Saying "could not read this device" is the smaller claim, and it is
 * the true one.
 */
export function vpnSessionsPlan(
  sel: VpnSelection,
  inventory: Readonly<Record<string, VpnInventory>>,
): VpnPlan {
  if (sel.nodes.length === 0) return { kind: 'empty' };
  if (sel.nodes.some((n) => inventory[n.nodeId] == null)) return { kind: 'loading' };

  const nodes: ResolvedVpnNode[] = [];
  const unsupported: string[] = [];
  const unreadable: string[] = [];
  for (const n of sel.nodes) {
    const entries = inventory[n.nodeId];
    if (entries === 'failed') {
      unreadable.push(nameOf(n));
      continue;
    }
    const entry = resolveVpnMetric(entries ?? []);
    if (!entry) {
      unsupported.push(nameOf(n));
      continue;
    }
    nodes.push({
      ...n,
      label: nameOf(n),
      metric: entry.metric,
      query: rangeOptsFor(metricView(entry.metric_kind, entry.dimension).chart),
    });
  }
  return { kind: 'chart', nodes, unsupported, unreadable };
}

/** One node's fetched history, paired with the resolved node it belongs to. A fetch that failed
 *  carries `range: null` — one unreachable device must not blank the whole chart. */
export interface VpnNodeSeries {
  node: ResolvedVpnNode;
  range: MetricRange | null;
}

/**
 * Did every node fail to answer — as opposed to answering with nothing to show?
 *
 * {@link buildVpnSeries} folds those two into the same empty result, and that is right for
 * *drawing*: neither has a line. It is wrong for the *sentence*. "No sessions yet" describes an idle
 * concentrator; a refused or failed request describes nothing about the device at all.
 *
 * 🚨 The cost of conflating them was measured on the sibling widget: ADR-123 shipped an allow-list
 * that refused every parameterized route, so Interface traffic was answered `401` for every
 * anonymous visitor on a public board — and reported it as quiet ports. The caller uses
 * `Promise.allSettled`, so `usePolled`'s error path is structurally unreachable here and the 401
 * reached no one.
 *
 * ⚠️ It says "nothing answered", never "you are not allowed": `allSettled` discards the reasons, so
 * a status code is not available at this point and naming one would be a guess.
 *
 * An empty selection is not a failure — there was nothing to ask.
 */
export function everyNodeFailed(entries: readonly VpnNodeSeries[]): boolean {
  return entries.length > 0 && entries.every((e) => e.range == null);
}

/**
 * Build the chart's shared x-axis and its series — one line per device.
 *
 * Two things here are load-bearing:
 *
 *  1. **Colour comes from the position in the *selection*, not among the nodes that answered.** A
 *     device whose fetch failed this tick must not shift the remaining lines onto each other's
 *     colours, which would silently re-attribute every line in the legend.
 *  2. **Each response carries its own `timestamps`.** The caller asks every node for the same
 *     `from`/`to`/`step`, so they normally agree — but "normally" is not a guarantee, and a mismatch
 *     would shift one device's history against the others while still drawing a plausible chart.
 *     Values are placed by timestamp, not by array position.
 *
 * The x-axis is the first node that returned data; nodes whose fetch failed contribute no series.
 */
export function buildVpnSeries(
  entries: readonly VpnNodeSeries[],
  palette: readonly string[],
): { timestamps: number[]; series: ChartSeries[] } {
  const axis = entries.find((e) => e.range != null && e.range.points.length > 0)?.range;
  if (!axis) return { timestamps: [], series: [] };
  const timestamps = axis.points.map((p) => p.t);
  const series: ChartSeries[] = [];

  entries.forEach((entry, i) => {
    const r = entry.range;
    if (r == null || r.points.length === 0) return;
    const byTs = new Map<number, number>();
    for (const p of r.points) byTs.set(p.t, p.v);
    series.push({
      label: entry.node.label,
      // A timestamp this device has no sample for is a gap, not a zero: drawing 0 would claim the
      // concentrator had nobody connected at a moment we simply did not measure.
      values: timestamps.map((ts) => byTs.get(ts) ?? null),
      color: palette[i % palette.length],
    });
  });

  return { timestamps, series };
}

/**
 * A dependency key for the *resolved* selection — what the fetch is actually about.
 *
 * {@link nodesKey} is not enough on its own: the metric a node resolves to can change under a fixed
 * set of node ids (an operator edits a collection set, the inventory is re-read, a
 * higher-priority candidate appears). Keying only on the ids would leave the widget polling the
 * previous vendor's metric name until it happened to remount.
 */
export function armedKey(nodes: readonly ResolvedVpnNode[]): string {
  return nodes.map((n) => `${n.nodeId}@${n.metric}`).join(',');
}

/** One device's current number, for the strip above the chart. */
export interface VpnReading {
  /** The React key. Two devices can share a display name; they cannot share an id. */
  nodeId: string;
  label: string;
  /** The metric this device's number came from — the caller turns it into the unit noun. */
  metric: string;
  /** The most recent sample, or `null` when the device has none (or did not answer). */
  value: number | null;
  /** The same colour as this device's line, so the two read as one thing. */
  color: string;
}

/**
 * The current reading per device, in selection order.
 *
 * The value is the **last sample of the series already fetched**, not a separate "current value"
 * request. That costs one fewer route in the widget's `reads` declaration (which is an access
 * control surface, ADR-123 決定 5) and guarantees the number and the right-hand end of the line
 * agree — two requests could disagree by a step and there would be nothing on screen to explain it.
 * The cost is that the number is up to one step old, which at this widget's windows is 60s at worst.
 *
 * ⚠️ Every selected device gets a row, including one that answered with nothing. A device that
 * silently vanished from a strip of six is indistinguishable from a device nobody selected.
 */
export function currentReadings(
  entries: readonly VpnNodeSeries[],
  palette: readonly string[],
): VpnReading[] {
  return entries.map((e, i) => ({
    nodeId: e.node.nodeId,
    label: e.node.label,
    metric: e.node.metric,
    value: latestOf(e.range),
    color: palette[i % palette.length],
  }));
}

/** The most recent sample of a range, or `null` when there is none.
 *
 *  Reads from the end rather than taking `points.at(-1)` blindly: the server returns points in
 *  ascending time, but a trailing gap is returned as an absent point rather than a null sample, so
 *  "the last element" and "the last thing measured" are the same only while the series is dense. */
export function latestOf(range: MetricRange | null): number | null {
  if (!range) return null;
  for (let i = range.points.length - 1; i >= 0; i -= 1) {
    const v = range.points[i].v;
    if (Number.isFinite(v)) return v;
  }
  return null;
}
