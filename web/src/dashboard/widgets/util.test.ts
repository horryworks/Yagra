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
  downCount,
  percentHealthy,
  stateCounts,
  topLevelRollup,
  worstState,
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
});

const group = (id: string, parent_id: string | null = null): NodeGroup => ({
  id,
  name: id,
  group_type: 'site',
  parent_id,
  sort_order: 0,
  latitude: null,
  longitude: null,
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
