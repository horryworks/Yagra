import { beforeEach, describe, expect, it } from 'vitest';
import { sortedAlerts, useAlertStore, useAuthStore, useRangeStore } from './store';
import type { Alert } from './types/api';

function alert(over: Partial<Alert>): Alert {
  return {
    node: 'n1',
    check: 'c1',
    severity: 'warning',
    state: 'warning',
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
