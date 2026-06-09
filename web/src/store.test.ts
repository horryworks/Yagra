import { beforeEach, describe, expect, it } from 'vitest';
import { sortedAlerts, useAlertStore } from './store';
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
