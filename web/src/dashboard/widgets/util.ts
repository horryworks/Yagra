// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers shared by the dashboard widgets (kept side-effect-free so they unit-test in the
// node env). State roll-ups, worst-state precedence, and alert-history bucketing live here.

import type {
  AlertHistoryRow,
  CalendarBucket,
  NodeGroup,
  NodeState,
  NodeSummary,
  TopologyNode,
} from '../../types/api';
// Worst-first precedence for rolling a set of node states up to a single "group" state.
import { SEVERITY_ORDER, emptyStateCounts } from '../../lib/nodeState';

/** The worst (most severe) state in a set, or `ok` when empty. Used for site/region tiles. */
export function worstState(states: NodeState[]): NodeState {
  for (const s of SEVERITY_ORDER) {
    if (states.includes(s)) return s;
  }
  return 'ok';
}

/** A per-state tally of a group's direct members — the `fleet/group-summary` value shape (A-1). */
export type StateCounts = Record<NodeState, number>;

/** The worst (most severe) state present in a per-state tally, or `ok` when the group is empty.
 *  The counts-driven twin of {@link worstState} for the server-side per-group rollup. */
export function worstStateFromCounts(c: StateCounts): NodeState {
  for (const s of SEVERITY_ORDER) {
    if ((c[s] ?? 0) > 0) return s;
  }
  return 'ok';
}

/** Sum of every state count = the group's direct-member total. */
export function countsTotal(c: StateCounts): number {
  return SEVERITY_ORDER.reduce((n, s) => n + (c[s] ?? 0), 0);
}

/** Count nodes by state. */
export function stateCounts(nodes: NodeSummary[]): Record<NodeState, number> {
  const counts = emptyStateCounts();
  for (const n of nodes) counts[n.state] += 1;
  return counts;
}

/** Nodes considered "down" for the KPI tile: hard-down states only (critical + unreachable). */
export function downCount(nodes: NodeSummary[]): number {
  return nodes.reduce((n, x) => (x.state === 'critical' || x.state === 'unreachable' ? n + 1 : n), 0);
}

/** Percent of nodes in `ok`, rounded; 0 when there are no nodes. */
export function percentHealthy(nodes: NodeSummary[]): number {
  if (nodes.length === 0) return 0;
  const ok = nodes.reduce((n, x) => (x.state === 'ok' ? n + 1 : n), 0);
  return Math.round((ok / nodes.length) * 100);
}

/** One hour bucket of alert-history "opened" events. `t` is the bucket's start (Unix ms). */
export interface HourBucket {
  t: number;
  count: number;
}

/** Bucket alert-history rows into `hours` trailing 1-hour bins by their open time, counting only
 *  fires (not resolves). `now` is injectable for deterministic tests. Oldest bucket first. */
export function bucketAlertsByHour(
  rows: AlertHistoryRow[],
  hours: number,
  now: number,
): HourBucket[] {
  const HOUR = 3_600_000;
  // Align "now" to the top of the current hour so bins are stable across calls within an hour.
  const end = Math.floor(now / HOUR) * HOUR + HOUR;
  const buckets: HourBucket[] = [];
  for (let i = hours - 1; i >= 0; i -= 1) {
    buckets.push({ t: end - (i + 1) * HOUR, count: 0 });
  }
  const start = buckets[0].t;
  for (const r of rows) {
    if (r.resolved) continue;
    if (r.at_unix_ms < start || r.at_unix_ms >= end) continue;
    const idx = Math.floor((r.at_unix_ms - start) / HOUR);
    if (idx >= 0 && idx < buckets.length) buckets[idx].count += 1;
  }
  return buckets;
}

/** A trailing window `{from,to}` in Unix **seconds** (for the flow widgets). Call inside the
 *  fetcher so each poll advances the window; `now` is injectable for deterministic tests. */
export function trailingSecs(spanSecs: number, now: number = Date.now()): { from: number; to: number } {
  const to = Math.floor(now / 1000);
  return { from: to - spanSecs, to };
}

/** A trailing window `{start,end}` as RFC-3339 strings (for the `/events/stats` widgets); `now`
 *  is injectable for deterministic tests. */
export function trailingIso(
  spanSecs: number,
  now: number = Date.now(),
): { start: string; end: string } {
  return { start: new Date(now - spanSecs * 1000).toISOString(), end: new Date(now).toISOString() };
}

/** Group `FlowPoint`s by protocol into aligned MetricChart series on a shared timestamp axis,
 *  keeping the top-N protocols by total bytes (N = palette length). Adapted for the fleet
 *  throughput-trend widget; `nameOf`/`palette` are injected to keep this layer free of chart deps. */
export function flowTrendSeries(
  points: { ts_unix_ms: number; proto: number; bytes: number }[],
  nameOf: (proto: number) => string,
  palette: string[],
): { timestamps: number[]; series: { label: string; values: (number | null)[]; color: string }[] } {
  if (points.length === 0) return { timestamps: [], series: [] };
  const tsSet = new Set<number>();
  const byProto = new Map<number, Map<number, number>>();
  const totals = new Map<number, number>();
  for (const p of points) {
    const ts = Math.floor(p.ts_unix_ms / 1000);
    tsSet.add(ts);
    let m = byProto.get(p.proto);
    if (!m) {
      m = new Map();
      byProto.set(p.proto, m);
    }
    m.set(ts, (m.get(ts) ?? 0) + p.bytes);
    totals.set(p.proto, (totals.get(p.proto) ?? 0) + p.bytes);
  }
  const timestamps = [...tsSet].sort((a, b) => a - b);
  const topProtos = [...totals.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, palette.length)
    .map(([proto]) => proto);
  const series = topProtos.map((proto, i) => {
    const m = byProto.get(proto) ?? new Map<number, number>();
    return {
      label: nameOf(proto),
      values: timestamps.map((ts) => m.get(ts) ?? null),
      color: palette[i % palette.length],
    };
  });
  return { timestamps, series };
}

/** Densify a sparse time-bucket series (as returned by `/events/stats?group_by=time`) into `bins`
 *  trailing bars of width `bucketMs`, oldest first, summing each source bucket into its bin — the
 *  event-volume histogram's input. `now` is injectable for deterministic tests. */
export function densifyTimeBuckets(
  buckets: { ts_unix_ms: number; count: number }[],
  bins: number,
  bucketMs: number,
  now: number = Date.now(),
): { t: number; count: number }[] {
  const end = Math.floor(now / bucketMs) * bucketMs + bucketMs;
  const out = Array.from({ length: bins }, (_, i) => ({ t: end - (bins - i) * bucketMs, count: 0 }));
  const start = out[0].t;
  for (const b of buckets) {
    const idx = Math.floor((b.ts_unix_ms - start) / bucketMs);
    if (idx >= 0 && idx < bins) out[idx].count += b.count;
  }
  return out;
}

/** A node in the dependency forest (parent → children). */
export interface TopoTreeNode {
  node: TopologyNode;
  children: TopoTreeNode[];
}

/** Build a parent→children forest from a flat topology node list. Nodes whose parent is missing
 *  (or absent) become roots; a self-parent is treated as a root. Each node appears once, so a
 *  parent cycle can't duplicate nodes — render with a depth cap as a belt-and-braces guard. */
export function buildForest(nodes: TopologyNode[]): TopoTreeNode[] {
  const byId = new Map<string, TopoTreeNode>(nodes.map((n) => [n.id, { node: n, children: [] }]));
  const roots: TopoTreeNode[] = [];
  for (const n of nodes) {
    const entry = byId.get(n.id)!;
    const parent = n.parent_id ? byId.get(n.parent_id) : undefined;
    if (parent && parent !== entry) parent.children.push(entry);
    else roots.push(entry);
  }
  return roots;
}

/** Expand sparse weekday×hour buckets into a dense 7×24 matrix (`m[dow][hour]`), zero-filled.
 *  For the alert-calendar heatmap. Out-of-range buckets are ignored. */
export function calendarMatrix(buckets: CalendarBucket[]): number[][] {
  const m: number[][] = Array.from({ length: 7 }, () => new Array(24).fill(0));
  for (const b of buckets) {
    if (b.dow >= 0 && b.dow < 7 && b.hour >= 0 && b.hour < 24) m[b.dow][b.hour] = b.count;
  }
  return m;
}

/** A per-region (top-level group) health roll-up. */
export interface RegionStat {
  id: string;
  name: string;
  total: number;
  up: number;
  /** Percent of members in `ok`, rounded. */
  pct: number;
}

/** Roll nodes up to their top-level group (walking parent links), counting % healthy per
 *  region. Nodes in a sub-group attribute to its top-level ancestor; ungrouped nodes are
 *  ignored. Cycle-guarded. Returns only regions with at least one member. */
export function topLevelRollup(nodes: NodeSummary[], groups: NodeGroup[]): RegionStat[] {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const topOf = (gid: string | null): string | null => {
    let cur = gid ? byId.get(gid) : undefined;
    const seen = new Set<string>();
    while (cur && cur.parent_id && byId.has(cur.parent_id)) {
      if (seen.has(cur.id)) {
        // A parent chain that loops back is a config error; stop and surface it instead of
        // silently attributing the node to an arbitrary mid-chain ancestor.
        console.warn('topLevelRollup: cycle in group hierarchy, stopping at', cur.id);
        break;
      }
      seen.add(cur.id);
      cur = byId.get(cur.parent_id);
    }
    return cur ? cur.id : null;
  };
  const stats = groups
    .filter((g) => g.parent_id === null)
    .map((t) => ({ id: t.id, name: t.name, total: 0, up: 0, pct: 0 }));
  const statById = new Map(stats.map((s) => [s.id, s]));
  for (const n of nodes) {
    const top = topOf(n.group_id);
    if (!top) continue;
    const s = statById.get(top);
    if (!s) continue;
    s.total += 1;
    if (n.state === 'ok') s.up += 1;
  }
  for (const s of stats) s.pct = s.total > 0 ? Math.round((s.up / s.total) * 100) : 0;
  return stats.filter((s) => s.total > 0);
}

/** Roll per-group DIRECT-member counts up to their top-level group (walking parent links), summing
 *  % healthy per region. The counts-driven twin of {@link topLevelRollup}: it drives off the
 *  server-side per-group tally (A-1) instead of a client-side node slice, so it aggregates the whole
 *  fleet, not the first page. Sub-group counts attribute to their top-level ancestor; a group with
 *  no resolvable top-level is ignored. Cycle-guarded. Returns only regions with ≥1 member. */
export function topLevelRollupFromCounts(
  counts: Record<string, StateCounts>,
  groups: NodeGroup[],
): RegionStat[] {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const topOf = (gid: string): string | null => {
    let cur: NodeGroup | undefined = byId.get(gid);
    const seen = new Set<string>();
    while (cur && cur.parent_id && byId.has(cur.parent_id)) {
      if (seen.has(cur.id)) {
        console.warn('topLevelRollupFromCounts: cycle in group hierarchy, stopping at', cur.id);
        break;
      }
      seen.add(cur.id);
      cur = byId.get(cur.parent_id);
    }
    return cur ? cur.id : null;
  };
  const stats = groups
    .filter((g) => g.parent_id === null)
    .map((t) => ({ id: t.id, name: t.name, total: 0, up: 0, pct: 0 }));
  const statById = new Map(stats.map((s) => [s.id, s]));
  for (const [gid, c] of Object.entries(counts)) {
    const top = topOf(gid);
    if (!top) continue;
    const s = statById.get(top);
    if (!s) continue;
    s.total += countsTotal(c);
    s.up += c.ok ?? 0;
  }
  for (const s of stats) s.pct = s.total > 0 ? Math.round((s.up / s.total) * 100) : 0;
  return stats.filter((s) => s.total > 0);
}
