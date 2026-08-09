// SPDX-License-Identifier: AGPL-3.0-only
// Pure helpers shared by the dashboard widgets (kept side-effect-free so they unit-test in the
// node env). State roll-ups, worst-state precedence, and alert-history bucketing live here.

import type {
  AlertHistoryRow,
  CalendarBucket,
  MetricTopAgg,
  NodeGroup,
  NodeState,
  NodeSummary,
  TopologyNode,
} from '../../types/api';
// Worst-first precedence for rolling a set of node states up to a single "group" state.
import { SEVERITY_ORDER, emptyStateCounts } from '../../lib/nodeState';
import type { WidgetSettings } from '../types';

/** The now / trailing-1h-peak window every Top-N widget stores in its settings bag, defaulting to
 *  `now`. Shared because the widgets that write it (`TopAggActions`) and the ones that plan a query
 *  from it are in different files, and an unrecognised value must land on the same default in both
 *  — the bag is user-editable JSON, so `agg: 'yesterday'` is a shape that can actually arrive. */
export function topAggOf(settings: WidgetSettings | undefined): MetricTopAgg {
  return settings?.agg === 'max_1h' ? 'max_1h' : 'now';
}

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

// Removed: `stateCounts` / `downCount` / `percentHealthy`, which tallied a client-side
// `NodeSummary[]`. The widgets read the server's `fleet/group-summary` rollup instead (it is the
// only shape that works at fleet scale), so `fleet.tsx` folds `HARD_DOWN_STATES` over
// `summary.states` directly and none of the three had a caller left.

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

// `buildForest` / `TopoTreeNode` were removed with ADR-043 Increment 2. They built a tree from the
// single `parent_id` column, and the dependency graph is no longer a tree: a node reached by two
// equal-length paths from a poller has two upstreams, which is the case the multi-parent
// suppression rule exists for. A forest cannot represent that — it would have had to pick one
// parent and silently drop the other, i.e. show the operator a graph the engine does not use.
//
// What replaced it is `rootCauseRows` below: the widget's question was always "what is actually
// broken", and a root-cause list answers it without claiming the structure is a tree.

/** One row of the root-cause summary: a node whose alert the engine has rolled up under an
 *  upstream, or an unattributed problem node. */
export interface RootCauseRow {
  /** The upstream being blamed, or the problem node itself when nothing was attributed. */
  node: TopologyNode;
  /** Nodes whose alerts are suppressed under `node`. Empty for an unattributed problem. */
  affected: TopologyNode[];
}

/**
 * Group the topology into causes and what they explain.
 *
 * Sorted by how much each cause explains, then by name, so the list is stable across refreshes and
 * the biggest incident is first. A node that is both a cause and itself suppressed appears once, as
 * the cause — the engine's `root_cause` already climbed to the top of the down chain, so re-nesting
 * would just re-derive what it decided.
 */
export function rootCauseRows(nodes: TopologyNode[]): RootCauseRow[] {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const affected = new Map<string, TopologyNode[]>();
  const suppressed = new Set<string>();
  for (const n of nodes) {
    if (!n.root_cause) continue;
    suppressed.add(n.id);
    affected.set(n.root_cause, [...(affected.get(n.root_cause) ?? []), n]);
  }
  const rows: RootCauseRow[] = [];
  for (const [causeId, kids] of affected) {
    const cause = byId.get(causeId);
    // A cause outside the caller's scope is not in the payload. Skipping the row is the honest
    // rendering — the same reason `topology_page` leaves an invisible parent's id dangling.
    if (cause) rows.push({ node: cause, affected: kids });
  }
  // Problems nothing explained: still the operator's business, and the state most of a fleet is in
  // while no dependencies are modelled at all.
  for (const n of nodes) {
    if (suppressed.has(n.id) || affected.has(n.id)) continue;
    if (n.state === 'critical' || n.state === 'unreachable') rows.push({ node: n, affected: [] });
  }
  rows.sort(
    (a, b) => b.affected.length - a.affected.length || a.node.name.localeCompare(b.node.name),
  );
  return rows;
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

/** Roll per-group DIRECT-member counts up to the map pin each group resolves to.
 *
 *  This is what makes a site pin mean "everything at this site". Before geo inheritance a pin
 *  showed its folder's *direct* members, so an operator who placed a site whose nodes all live in
 *  rack sub-folders saw a pin with nothing in it.
 *
 *  Deliberately not a tree walk: the server already resolved "which pin does this folder belong
 *  to" into `geo_group` (one place, `groups.rs::resolve_group_geo`), so this is a fold. Writing the
 *  nearest-placed-ancestor walk again here is what would let the map and the server disagree about
 *  where a folder is — the same failure the region rollup above still risks by walking `parent_id`
 *  itself. Groups that resolve to no pin are skipped. */
export function pinRollupFromCounts(
  counts: Record<string, StateCounts>,
  groups: NodeGroup[],
): Record<string, StateCounts> {
  const out: Record<string, StateCounts> = {};
  for (const g of groups) {
    const pin = g.geo_group;
    if (!pin) continue;
    const c = counts[g.id];
    if (!c) continue;
    const acc = (out[pin] ??= emptyStateCounts());
    for (const s of SEVERITY_ORDER) acc[s] += c[s] ?? 0;
  }
  return out;
}

/** Roll nodes up to their top-level group (walking parent links), counting % healthy per
 *  region. Nodes in a sub-group attribute to its top-level ancestor; ungrouped nodes are
 *  ignored. Cycle-guarded. Returns only regions with at least one member. */
export function topLevelRollup(nodes: NodeSummary[], groups: NodeGroup[]): RegionStat[] {
  const byId = new Map(groups.map((g) => [g.id, g]));
  const topOf = (gid: string | null | undefined): string | null => {
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
