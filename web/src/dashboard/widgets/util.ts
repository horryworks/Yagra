// Pure helpers shared by the dashboard widgets (kept side-effect-free so they unit-test in the
// node env). State roll-ups, worst-state precedence, and alert-history bucketing live here.

import type {
  AlertHistoryRow,
  CalendarBucket,
  NodeGroup,
  NodeState,
  NodeSummary,
} from '../../types/api';

/** Worst-first precedence for rolling a set of node states up to a single "group" state. */
const SEVERITY_ORDER: NodeState[] = [
  'critical',
  'unreachable',
  'warning',
  'unknown',
  'maintenance',
  'ok',
];

/** The worst (most severe) state in a set, or `ok` when empty. Used for site/region tiles. */
export function worstState(states: NodeState[]): NodeState {
  for (const s of SEVERITY_ORDER) {
    if (states.includes(s)) return s;
  }
  return 'ok';
}

/** Count nodes by state. */
export function stateCounts(nodes: NodeSummary[]): Record<NodeState, number> {
  const counts: Record<NodeState, number> = {
    ok: 0,
    warning: 0,
    critical: 0,
    unknown: 0,
    unreachable: 0,
    maintenance: 0,
  };
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
    while (cur && cur.parent_id && byId.has(cur.parent_id) && !seen.has(cur.id)) {
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
