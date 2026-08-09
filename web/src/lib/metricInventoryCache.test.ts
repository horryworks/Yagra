// SPDX-License-Identifier: AGPL-3.0-only
// The memory decides what the fleet Top-N widget will offer to rank, so the rule that matters is
// which sighting wins when two nodes describe the same metric differently. Getting that backwards
// puts a raw counter in the picker, and a ranked list of odometer readings looks exactly like a
// ranked list of measurements.

import { describe, expect, it } from 'vitest';
import { mergeMetricKinds, metricKindsSnapshot, rememberMetrics, subscribeMetricKinds } from './metricInventoryCache';
import type { MetricKind, NodeMetricEntry } from '../types/api';

const entry = (metric: string, over: Partial<NodeMetricEntry> = {}): NodeMetricEntry => ({
  metric,
  metric_kind: 'gauge',
  dimension: 'none',
  status: 'ok',
  series_count: 1,
  ...over,
});

const empty = (): ReadonlyMap<string, MetricKind> => new Map();

describe('mergeMetricKinds', () => {
  it('remembers each metric with the kind the node reported', () => {
    const m = mergeMetricKinds(empty(), [
      entry('huawei_cpu_usage'),
      entry('if_hc_in_octets', { metric_kind: 'counter', dimension: 'interface' }),
    ]);
    expect(m.get('huawei_cpu_usage')).toBe('gauge');
    expect(m.get('if_hc_in_octets')).toBe('counter');
  });

  it('returns the previous map unchanged when it learned nothing', () => {
    // Identity, not equality: a `useSyncExternalStore` reader compares references, so a fresh map
    // per sighting would re-render every dashboard widget on every inventory fetch.
    const first = mergeMetricKinds(empty(), [entry('juniper_temp_c')]);
    expect(mergeMetricKinds(first, [entry('juniper_temp_c')])).toBe(first);
    expect(mergeMetricKinds(first, [])).toBe(first);
  });

  it('keeps a counter sighting when a later node calls the same metric a gauge', () => {
    // The case is not hypothetical: the inventory reports a metric with no collection item as a
    // gauge, so a node whose profile dropped the item — while its series is still in the store —
    // describes a real counter as a gauge. Refusing to rank it is the safe way to be wrong.
    const seen = mergeMetricKinds(empty(), [entry('if_in_errors', { metric_kind: 'counter' })]);
    const later = mergeMetricKinds(seen, [entry('if_in_errors', { metric_kind: 'gauge' })]);
    expect(later.get('if_in_errors')).toBe('counter');
    expect(later).toBe(seen);
  });

  it('upgrades a gauge sighting to a counter', () => {
    const seen = mergeMetricKinds(empty(), [entry('if_in_errors')]);
    expect(mergeMetricKinds(seen, [entry('if_in_errors', { metric_kind: 'counter' })]).get('if_in_errors')).toBe(
      'counter',
    );
  });
});

describe('the session memory', () => {
  it('records what a node reported and wakes subscribers once', () => {
    let woken = 0;
    const stop = subscribeMetricKinds(() => {
      woken += 1;
    });
    rememberMetrics([entry('fortinet_sslvpn_users')]);
    rememberMetrics([entry('fortinet_sslvpn_users')]);
    stop();
    expect(metricKindsSnapshot().get('fortinet_sslvpn_users')).toBe('gauge');
    expect(woken).toBe(1);
  });
});
