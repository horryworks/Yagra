// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type {
  AlertHistoryRow,
  CalendarBucket,
  NodeGroup,
  NodeSummary,
  TopologyNode,
} from '../../types/api';
import {
  bucketAlertsByHour,
  buildForest,
  calendarMatrix,
  countsTotal,
  densifyTimeBuckets,
  downCount,
  flowTrendSeries,
  percentHealthy,
  type StateCounts,
  stateCounts,
  topLevelRollup,
  topLevelRollupFromCounts,
  trailingIso,
  trailingSecs,
  worstState,
  worstStateFromCounts,
} from './util';

const node = (id: string, state: NodeSummary['state'], group_id: string | null = null): NodeSummary => ({
  id,
  name: id,
  address: '10.0.0.1',
  state,
  vendor: null,
  model: null,
  group_id,
  sort_order: 0,
  source: 'device',
});

const group = (id: string, parent_id: string | null = null): NodeGroup => ({
  id,
  name: id,
  group_type: 'site',
  parent_id,
  sort_order: 0,
  latitude: null,
  longitude: null,
  pool: null,
});

describe('state roll-ups', () => {
  it('picks the worst state by precedence', () => {
    expect(worstState(['ok', 'warning', 'critical'])).toBe('critical');
    expect(worstState(['ok', 'unknown'])).toBe('unknown');
    expect(worstState(['ok', 'maintenance'])).toBe('maintenance');
    expect(worstState([])).toBe('ok');
  });

  it('counts states and down/healthy derivations', () => {
    const nodes = [node('a', 'ok'), node('b', 'critical'), node('c', 'unreachable'), node('d', 'ok')];
    const c = stateCounts(nodes);
    expect(c.ok).toBe(2);
    expect(c.critical).toBe(1);
    expect(c.unreachable).toBe(1);
    expect(downCount(nodes)).toBe(2); // critical + unreachable
    expect(percentHealthy(nodes)).toBe(50);
    expect(percentHealthy([])).toBe(0);
  });
});

describe('bucketAlertsByHour', () => {
  it('bins fires into trailing hour buckets and ignores resolves/out-of-range', () => {
    const HOUR = 3_600_000;
    // Pin "now" to a hour boundary so bins are deterministic.
    const now = 100 * HOUR + 123;
    const row = (offsetHours: number, resolved = false): AlertHistoryRow => ({
      node: 'n',
      check: 'c',
      severity: 'critical',
      state: 'critical',
      at_unix_ms: 100 * HOUR - offsetHours * HOUR + 60_000, // one minute into that hour
      resolved,
      recorded_at: '1970-01-01T00:00:00Z', // unused by bucketing; required by the type
    });
    const buckets = bucketAlertsByHour(
      [row(0), row(0), row(1), row(2, true), row(30) /* out of 24h range */],
      24,
      now,
    );
    expect(buckets).toHaveLength(24);
    // Most recent bucket (last) holds the two current-hour fires.
    expect(buckets[buckets.length - 1].count).toBe(2);
    // One hour back holds one fire; the resolved row and the 30h-old row are excluded.
    expect(buckets[buckets.length - 2].count).toBe(1);
    const total = buckets.reduce((n, b) => n + b.count, 0);
    expect(total).toBe(3);
  });
});

describe('buildForest', () => {
  const tnode = (id: string, parent_id: string | null, root_cause: string | null = null): TopologyNode => ({
    id,
    name: id,
    parent_id,
    state: 'ok',
    root_cause,
  });

  it('nests children under parents and keeps parentless nodes as roots', () => {
    const forest = buildForest([
      tnode('core', null),
      tnode('dist', 'core'),
      tnode('access', 'dist'),
      tnode('island', null),
    ]);
    expect(forest.map((r) => r.node.id).sort()).toEqual(['core', 'island']);
    const core = forest.find((r) => r.node.id === 'core')!;
    expect(core.children.map((c) => c.node.id)).toEqual(['dist']);
    expect(core.children[0].children.map((c) => c.node.id)).toEqual(['access']);
  });

  it('treats a missing parent and a self-parent as roots (no dangling/loop)', () => {
    const forest = buildForest([tnode('a', 'ghost'), tnode('b', 'b')]);
    expect(forest.map((r) => r.node.id).sort()).toEqual(['a', 'b']);
  });
});

describe('calendarMatrix', () => {
  it('expands sparse buckets into a dense 7×24 matrix and ignores out-of-range', () => {
    const buckets: CalendarBucket[] = [
      { dow: 0, hour: 0, count: 3 },
      { dow: 6, hour: 23, count: 5 },
      { dow: 9, hour: 0, count: 99 }, // out of range ⇒ ignored
    ];
    const m = calendarMatrix(buckets);
    expect(m.length).toBe(7);
    expect(m[0].length).toBe(24);
    expect(m[0][0]).toBe(3);
    expect(m[6][23]).toBe(5);
    expect(m[3][12]).toBe(0); // zero-filled
  });
});

// The server-side per-group value shape (A-1): every state key present.
const counts = (partial: Partial<StateCounts>): StateCounts => ({
  ok: 0,
  warning: 0,
  critical: 0,
  unreachable: 0,
  maintenance: 0,
  unknown: 0,
  ...partial,
});

describe('counts-driven roll-ups (server-side per-group summary, A-1)', () => {
  it('worstStateFromCounts picks the worst present state, ok when empty', () => {
    expect(worstStateFromCounts(counts({ ok: 3, warning: 1, critical: 2 }))).toBe('critical');
    expect(worstStateFromCounts(counts({ ok: 2, unknown: 1 }))).toBe('unknown');
    expect(worstStateFromCounts(counts({ ok: 1, maintenance: 1 }))).toBe('maintenance');
    expect(worstStateFromCounts(counts({}))).toBe('ok');
  });

  it('countsTotal sums every state', () => {
    expect(countsTotal(counts({ ok: 3, warning: 1, unreachable: 2 }))).toBe(6);
    expect(countsTotal(counts({}))).toBe(0);
  });

  it('topLevelRollupFromCounts attributes sub-group counts to their top-level region', () => {
    const groups = [group('tokyo'), group('rackA', 'tokyo'), group('osaka')];
    const groupCounts: Record<string, StateCounts> = {
      tokyo: counts({ ok: 1 }),
      rackA: counts({ critical: 1 }), // sub-group rolls up to tokyo
      osaka: counts({ ok: 1 }),
    };
    const stats = topLevelRollupFromCounts(groupCounts, groups);
    const tokyo = stats.find((s) => s.id === 'tokyo')!;
    const osaka = stats.find((s) => s.id === 'osaka')!;
    expect(tokyo.total).toBe(2);
    expect(tokyo.up).toBe(1);
    expect(tokyo.pct).toBe(50);
    expect(osaka.pct).toBe(100);
    // Only regions with members are returned.
    expect(stats.every((s) => s.total > 0)).toBe(true);
  });

  it('topLevelRollupFromCounts ignores counts for orphan groups (no top-level ancestor)', () => {
    // A group whose id isn't in the group list resolves to no top-level → contributes nothing.
    const groups = [group('tokyo')];
    const stats = topLevelRollupFromCounts({ ghost: counts({ critical: 5 }) }, groups);
    expect(stats).toEqual([]);
  });
});

describe('topLevelRollup', () => {
  it('attributes nodes (incl. sub-groups) to their top-level region', () => {
    const groups = [group('tokyo'), group('rackA', 'tokyo'), group('osaka')];
    const nodes = [
      node('a', 'ok', 'tokyo'),
      node('b', 'critical', 'rackA'), // sub-group rolls up to tokyo
      node('c', 'ok', 'osaka'),
      node('d', 'ok', null), // ungrouped — ignored
    ];
    const stats = topLevelRollup(nodes, groups);
    const tokyo = stats.find((s) => s.id === 'tokyo')!;
    const osaka = stats.find((s) => s.id === 'osaka')!;
    expect(tokyo.total).toBe(2);
    expect(tokyo.up).toBe(1);
    expect(tokyo.pct).toBe(50);
    expect(osaka.pct).toBe(100);
    // Only regions with members are returned (no empty top-level groups).
    expect(stats.every((s) => s.total > 0)).toBe(true);
  });
});

describe('flow / event widget helpers', () => {
  it('trailingSecs returns a window ending at `now` (seconds)', () => {
    const now = 1_700_000_000_000; // fixed ms
    const w = trailingSecs(3600, now);
    expect(w.to).toBe(1_700_000_000);
    expect(w.from).toBe(1_700_000_000 - 3600);
  });

  it('trailingIso returns RFC-3339 start/end bracketing the span', () => {
    const now = 1_700_000_000_000;
    const w = trailingIso(86_400, now);
    expect(w.end).toBe(new Date(now).toISOString());
    expect(w.start).toBe(new Date(now - 86_400_000).toISOString());
  });

  it('densifyTimeBuckets fills dense trailing bins and drops out-of-range buckets', () => {
    const now = 10 * 3_600_000; // aligned to an hour boundary
    const bins = densifyTimeBuckets(
      [
        { ts_unix_ms: now, count: 3 }, // current hour → newest (last) bin
        { ts_unix_ms: now - 3_600_000, count: 5 }, // previous hour
        { ts_unix_ms: now - 100 * 3_600_000, count: 99 }, // far past — dropped
      ],
      24,
      3_600_000,
      now,
    );
    expect(bins).toHaveLength(24);
    expect(bins.reduce((n, b) => n + b.count, 0)).toBe(8); // only the two in-range buckets
    expect(bins[bins.length - 1].count).toBe(3); // newest bin (current hour)
    expect(bins[bins.length - 2].count).toBe(5); // previous hour
  });

  it('flowTrendSeries aligns per-protocol series on a shared timestamp axis, top-N by bytes', () => {
    const nameOf = (p: number) => (p === 6 ? 'TCP' : p === 17 ? 'UDP' : `IP ${p}`);
    const palette = ['#a', '#b'];
    const { timestamps, series } = flowTrendSeries(
      [
        { ts_unix_ms: 1000, proto: 6, bytes: 100 },
        { ts_unix_ms: 2000, proto: 6, bytes: 50 },
        { ts_unix_ms: 1000, proto: 17, bytes: 10 },
      ],
      nameOf,
      palette,
    );
    expect(timestamps).toEqual([1, 2]); // seconds, sorted
    expect(series[0].label).toBe('TCP'); // higher total bytes first
    expect(series[0].values).toEqual([100, 50]);
    expect(series[1].label).toBe('UDP');
    expect(series[1].values).toEqual([10, null]); // gap-filled where absent
  });

  it('flowTrendSeries is empty for no points', () => {
    expect(flowTrendSeries([], (p) => String(p), ['#a'])).toEqual({ timestamps: [], series: [] });
  });
});
