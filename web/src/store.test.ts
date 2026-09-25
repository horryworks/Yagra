// SPDX-License-Identifier: AGPL-3.0-only
import { beforeEach, describe, expect, it } from 'vitest';
import {
  sortedAlerts,
  useAlertStore,
  useAuthStore,
  useLastBoardStore,
  useMapViewStore,
  useNodeTabStore,
  useRangeStore,
  useSectionRouteStore,
} from './store';
import type { Alert } from './types/api';

function alert(over: Partial<Alert>): Alert {
  return {
    node: 'n1',
    subject_kind: 'node',
    check: 'c1',
    severity: 'warning',
    state: 'warning',
    metric: 'icmp_rtt_ms',
    at_unix_ms: 0,
    root_cause: null,
    flapping: false,
    ...over,
  };
}

beforeEach(() => {
  useAlertStore.getState().clear();
});

describe('alert store', () => {
  it('upserts by dedup identity (no duplicates)', () => {
    const s = useAlertStore.getState();
    s.upsertAlert(alert({ at_unix_ms: 1 }));
    s.upsertAlert(alert({ at_unix_ms: 2 })); // same node|check|severity
    expect(Object.keys(useAlertStore.getState().alerts)).toHaveLength(1);
    expect(Object.values(useAlertStore.getState().alerts)[0].at_unix_ms).toBe(2);
  });

  it('resolves an alert by key', () => {
    const s = useAlertStore.getState();
    s.upsertAlert(alert({}));
    s.resolveAlert({ node: 'n1', check: 'c1', severity: 'warning' });
    expect(Object.keys(useAlertStore.getState().alerts)).toHaveLength(0);
  });

  it('setAlerts replaces the set: an alert missing from the snapshot drops out (ADR-019 増分 1)', () => {
    const s = useAlertStore.getState();
    s.upsertAlert(alert({ node: 'ghost' })); // resolved while the stream was down
    s.upsertAlert(alert({ node: 'kept', at_unix_ms: 1 }));
    s.setAlerts([alert({ node: 'kept', at_unix_ms: 7 }), alert({ node: 'new' })]);
    const after = useAlertStore.getState().alerts;
    expect(Object.values(after).map((a) => a.node).sort()).toEqual(['kept', 'new']);
    expect(Object.values(after).find((a) => a.node === 'kept')?.at_unix_ms).toBe(7);
  });

  it('setAlerts keys the snapshot the way upsert does (a duplicate collapses)', () => {
    useAlertStore.getState().setAlerts([alert({ at_unix_ms: 1 }), alert({ at_unix_ms: 2 })]);
    expect(Object.values(useAlertStore.getState().alerts).map((a) => a.at_unix_ms)).toEqual([2]);
  });

  it('sorts worst-first then most-recent-first', () => {
    const list = sortedAlerts({
      a: alert({ node: 'a', severity: 'warning', at_unix_ms: 5 }),
      b: alert({ node: 'b', severity: 'critical', at_unix_ms: 1 }),
      c: alert({ node: 'c', severity: 'critical', at_unix_ms: 9 }),
    });
    expect(list.map((x) => x.node)).toEqual(['c', 'b', 'a']);
  });
});

describe('auth store', () => {
  it('tracks the logged-in flag', () => {
    useAuthStore.getState().setAuthed(true);
    expect(useAuthStore.getState().authed).toBe(true);
    useAuthStore.getState().setAuthed(false);
    expect(useAuthStore.getState().authed).toBe(false);
  });
});

describe('range store', () => {
  it('holds one shared range that every consumer reads/writes', () => {
    useRangeStore.getState().setRange({ kind: 'relative', secs: 6 * 3600 });
    expect(useRangeStore.getState().range).toEqual({ kind: 'relative', secs: 6 * 3600 });
    useRangeStore.getState().setRange({ kind: 'absolute', from: 1000, to: 2000 });
    expect(useRangeStore.getState().range).toEqual({ kind: 'absolute', from: 1000, to: 2000 });
  });
});

// The three session memories added by ADR-134. Each one is read by a `.tsx`, so the store is the
// only place their judgement can be tested at all.
describe('node-detail tab memory (ADR-134)', () => {
  beforeEach(() => useNodeTabStore.setState({ tab: 'overview' }));

  it('starts on Overview, so a fresh session behaves exactly as before', () => {
    expect(useNodeTabStore.getState().tab).toBe('overview');
  });

  it('remembers a tab that was clicked', () => {
    useNodeTabStore.getState().rememberTab('interfaces');
    expect(useNodeTabStore.getState().tab).toBe('interfaces');
  });

  // A session written by another build, or hand-edited storage, must not be able to park the whole
  // app on a tab this build cannot render.
  it('normalizes on the way in, so an unknown tab cannot be stored', () => {
    useNodeTabStore.getState().rememberTab('sensors');
    expect(useNodeTabStore.getState().tab).toBe('overview');
  });
});

describe('last dashboard board memory (ADR-134)', () => {
  beforeEach(() => useLastBoardStore.setState({ byBoard: {} }));

  it('remembers nothing until a board is shown, so the first board still wins', () => {
    expect(useLastBoardStore.getState().byBoard).toEqual({});
  });

  // Keyed per dashboard: the three documents have separate board sets, so one shared key would name
  // a board the other two have never heard of.
  it('keeps one board per dashboard', () => {
    useLastBoardStore.getState().rememberBoard('my', 'b2');
    useLastBoardStore.getState().rememberBoard('shared', 'b9');
    expect(useLastBoardStore.getState().byBoard).toEqual({ my: 'b2', shared: 'b9' });
  });

  // Re-recording the board already stored returns the same state object, so a `load()` on every
  // mount cannot make subscribers re-render over an unchanged value.
  it('does not churn state when the board has not changed', () => {
    useLastBoardStore.getState().rememberBoard('my', 'b2');
    const before = useLastBoardStore.getState().byBoard;
    useLastBoardStore.getState().rememberBoard('my', 'b2');
    expect(useLastBoardStore.getState().byBoard).toBe(before);
  });
});

describe('map view memory (ADR-134)', () => {
  beforeEach(() => useMapViewStore.setState({ topo: null, geo: null }));

  it('starts unpositioned on both maps, so the first measured frame still auto-fits', () => {
    expect(useMapViewStore.getState().topo).toBeNull();
    expect(useMapViewStore.getState().geo).toBeNull();
  });

  // ⚠️ The two maps are transforms over different things (a laid-out diagram vs. a world
  // projection), so one memory must never answer for the other.
  it('keeps the two maps apart', () => {
    useMapViewStore.getState().setMapView('topo', { tx: 10, ty: 20, scale: 1.5 });
    expect(useMapViewStore.getState().topo).toEqual({ tx: 10, ty: 20, scale: 1.5 });
    expect(useMapViewStore.getState().geo).toBeNull();
  });

  // The judgement this store exists to hold: every wheel and pinch handler in both maps passes an
  // updater, and resolving it here is what keeps it out of a `.tsx` where no test could run it.
  it('resolves an updater against the stored view', () => {
    useMapViewStore.getState().setMapView('geo', { tx: 0, ty: 0, scale: 1 });
    useMapViewStore.getState().setMapView('geo', (v) => (v ? { ...v, scale: v.scale * 2 } : v));
    expect(useMapViewStore.getState().geo).toEqual({ tx: 0, ty: 0, scale: 2 });
  });

  // Both maps' handlers open with `if (!v) return v`, so the updater has to survive being handed a
  // null — that is the "never positioned" state, not an error.
  it('hands an updater the null it expects before the first fit', () => {
    let seen: unknown = 'not called';
    useMapViewStore.getState().setMapView('topo', (v) => {
      seen = v;
      return v;
    });
    expect(seen).toBeNull();
    expect(useMapViewStore.getState().topo).toBeNull();
  });

  it('clears back to unpositioned', () => {
    useMapViewStore.getState().setMapView('topo', { tx: 1, ty: 2, scale: 1 });
    useMapViewStore.getState().setMapView('topo', null);
    expect(useMapViewStore.getState().topo).toBeNull();
  });
});

describe('nav route memory (ADR-134 増分 2 and 3)', () => {
  beforeEach(() => useSectionRouteStore.setState({ bySection: {}, byItem: {} }));
  const remember = (section: string, item: string, route: string) =>
    useSectionRouteStore.getState().rememberRoute(section, item, route);

  it('remembers nothing until a section is visited, so its landing child still wins', () => {
    expect(useSectionRouteStore.getState().bySection).toEqual({});
    expect(useSectionRouteStore.getState().byItem).toEqual({});
  });

  // One entry per section: the seven tabs are seven independent destinations, and a shared key
  // would send Nodes to a dashboard.
  it('keeps one route per section', () => {
    remember('dashboard', '/dashboard/my', '/dashboard/my');
    remember('events', '/events', '/events?message=router');
    expect(useSectionRouteStore.getState().bySection).toEqual({
      dashboard: '/dashboard/my',
      events: '/events?message=router',
    });
  });

  it('replaces a section’s route when the operator moves within it', () => {
    remember('nodes', '/nodes', '/nodes');
    remember('nodes', '/nodes/credentials', '/nodes/credentials');
    expect(useSectionRouteStore.getState().bySection).toEqual({ nodes: '/nodes/credentials' });
  });

  // 増分 3 — the reported symptom: search All nodes, open Discovery. The section now points at
  // Discovery, and the item memory still holds All nodes' term for the sidebar link back.
  it('keeps one route per item, which a move to a sibling item does not overwrite', () => {
    remember('nodes', '/nodes', '/nodes?q=sw');
    remember('nodes', '/nodes/discovery', '/nodes/discovery');
    expect(useSectionRouteStore.getState().bySection).toEqual({ nodes: '/nodes/discovery' });
    expect(useSectionRouteStore.getState().byItem).toEqual({
      '/nodes': '/nodes?q=sw',
      '/nodes/discovery': '/nodes/discovery',
    });
  });

  // The effect that writes this runs on every route change, and a filter can rewrite the query on
  // every keystroke — so an unchanged value must return the same state object and re-render nothing.
  it('does not churn state when the route has not changed', () => {
    remember('nodes', '/nodes/mib', '/nodes/mib');
    const before = useSectionRouteStore.getState();
    remember('nodes', '/nodes/mib', '/nodes/mib');
    expect(useSectionRouteStore.getState().bySection).toBe(before.bySection);
    expect(useSectionRouteStore.getState().byItem).toBe(before.byItem);
  });
});
