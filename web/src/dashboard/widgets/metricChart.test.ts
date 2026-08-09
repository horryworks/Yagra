// SPDX-License-Identifier: AGPL-3.0-only
// The widget's two failure modes are a stale persisted selection and a wrong query shape, and
// neither one looks broken on screen: the first draws an empty chart, the second draws a plausible
// one. Both are decided here.

import { describe, expect, it } from 'vitest';
import {
  chartableMetrics,
  metricChartPlan,
  readSelection,
  type MetricChartSelection,
} from './metricChart';
import { METRIC_DIMENSIONS, METRIC_KINDS } from '../../types/api';
import type { MetricDimension, MetricKind, NodeMetricEntry } from '../../types/api';

const entry = (metric: string, over: Partial<NodeMetricEntry> = {}): NodeMetricEntry => ({
  metric,
  metric_kind: 'gauge',
  dimension: 'none',
  status: 'ok',
  series_count: 1,
  ...over,
});

const sel = (over: Partial<MetricChartSelection> = {}): MetricChartSelection => ({
  nodeId: 'n1',
  nodeName: 'core-sw-01',
  metric: 'huawei_cpu_usage',
  ...over,
});

describe('readSelection', () => {
  it('reads a complete selection', () => {
    expect(readSelection({ nodeId: 'n1', nodeName: 'core-sw-01', metric: 'm' })).toEqual({
      nodeId: 'n1',
      nodeName: 'core-sw-01',
      metric: 'm',
    });
  });

  it('treats a missing bag as nothing picked', () => {
    expect(readSelection(undefined)).toEqual({ nodeId: null, nodeName: null, metric: null });
    expect(readSelection({})).toEqual({ nodeId: null, nodeName: null, metric: null });
  });

  it('refuses a non-string where an id belongs', () => {
    // The layout document is user-editable JSON that round-trips through storage; a number here
    // must degrade to "nothing picked", not become a request for `/nodes/42/metrics`.
    expect(readSelection({ nodeId: 42, metric: { a: 1 }, nodeName: null })).toEqual({
      nodeId: null,
      nodeName: null,
      metric: null,
    });
  });

  it('treats an empty string as nothing picked', () => {
    expect(readSelection({ nodeId: '', metric: '' }).nodeId).toBeNull();
  });
});

describe('chartableMetrics', () => {
  it('offers node-level gauges and counters', () => {
    const rows = [entry('huawei_cpu_usage'), entry('if_errors_total', { metric_kind: 'counter' })];
    expect(chartableMetrics(rows).map((e) => e.metric)).toEqual([
      'huawei_cpu_usage',
      'if_errors_total',
    ]);
  });

  it('offers a per-entity gauge (collapsed to the node max) but never a per-entity counter', () => {
    // The rows of a folded multi-index table cannot be named, so there is no per-row rate to take
    // and nothing to collapse afterwards.
    const rows = [
      entry('huawei_mem_used', { dimension: 'entity' }),
      entry('huawei_slot_drops', { dimension: 'entity', metric_kind: 'counter' }),
    ];
    expect(chartableMetrics(rows).map((e) => e.metric)).toEqual(['huawei_mem_used']);
  });

  it('never offers an interface-dimensioned metric', () => {
    // Those are charted per row by the Interfaces tab; one line for eight ports answers a
    // different question.
    const rows = [
      entry('if_hc_in_octets', { dimension: 'interface', metric_kind: 'counter' }),
      entry('if_oper_status', { dimension: 'interface' }),
    ];
    expect(chartableMetrics(rows)).toEqual([]);
  });

  it('still offers a configured metric that has produced nothing', () => {
    // `no_data` is "configured and silent". An empty chart says that; hiding the row would read as
    // "you never configured this".
    const rows = [entry('juniper_temp_c', { status: 'no_data', series_count: 0 })];
    expect(chartableMetrics(rows).map((e) => e.metric)).toEqual(['juniper_temp_c']);
  });
});

describe('metricChartPlan', () => {
  it('asks for a node first', () => {
    expect(metricChartPlan(sel({ nodeId: null, metric: null }), null)).toEqual({ kind: 'pick-node' });
    // A metric persisted without a node is still "pick a node" — not an attempt to query one.
    expect(metricChartPlan(sel({ nodeId: null }), null)).toEqual({ kind: 'pick-node' });
  });

  it('distinguishes an inventory that has not arrived from a node with no metrics', () => {
    // Conflating the two would flash "not available" on every node switch.
    expect(metricChartPlan(sel(), null)).toEqual({ kind: 'loading' });
    expect(metricChartPlan(sel(), [])).toEqual({
      kind: 'unavailable',
      metric: 'huawei_cpu_usage',
    });
  });

  it('asks for a metric once the inventory is in', () => {
    expect(metricChartPlan(sel({ metric: null }), [entry('huawei_cpu_usage')])).toEqual({
      kind: 'pick-metric',
    });
  });

  it('reports a persisted metric the node no longer has', () => {
    // The board outlives the collection set: an operator removes an item, or re-points the widget
    // at a device that never had that OID.
    expect(metricChartPlan(sel({ metric: 'gone_metric' }), [entry('huawei_cpu_usage')])).toEqual({
      kind: 'unavailable',
      metric: 'gone_metric',
    });
  });

  it('reports a persisted metric that is no longer chartable here', () => {
    // Same shape as above, and the one that matters more: the metric exists and has data, so the
    // widget would happily draw something wrong if it only checked for presence.
    const rows = [entry('if_hc_in_octets', { dimension: 'interface', metric_kind: 'counter' })];
    expect(metricChartPlan(sel({ metric: 'if_hc_in_octets' }), rows)).toEqual({
      kind: 'unavailable',
      metric: 'if_hc_in_octets',
    });
  });

  it('charts a node-level gauge with no query parameters', () => {
    expect(metricChartPlan(sel(), [entry('huawei_cpu_usage')])).toEqual({
      kind: 'chart',
      nodeId: 'n1',
      metric: 'huawei_cpu_usage',
      query: {},
      perSecond: false,
    });
  });

  it('charts a node-level counter as a rate, never as its stored values', () => {
    // The ADR-012 accident: a raw counter drawn directly is a smooth rising line that reads as
    // growth. This is the only cell where `rate` is correct, and the only one that may set it.
    const rows = [entry('snmp_in_discards', { metric_kind: 'counter' })];
    expect(metricChartPlan(sel({ metric: 'snmp_in_discards' }), rows)).toEqual({
      kind: 'chart',
      nodeId: 'n1',
      metric: 'snmp_in_discards',
      query: { rate: true },
      perSecond: true,
    });
  });

  it('collapses a per-entity gauge with agg=max', () => {
    const rows = [entry('huawei_mem_used', { dimension: 'entity', series_count: 4 })];
    expect(metricChartPlan(sel({ metric: 'huawei_mem_used' }), rows)).toEqual({
      kind: 'chart',
      nodeId: 'n1',
      metric: 'huawei_mem_used',
      query: { agg: 'max' },
      perSecond: false,
    });
  });

  it('never emits rate and agg together, for any metric the picker can offer', () => {
    // The server rejects the pair with a typed 400; this asserts the client cannot construct it in
    // the first place, across every kind × dimension the inventory can produce.
    for (const kind of METRIC_KINDS as readonly MetricKind[]) {
      for (const dimension of METRIC_DIMENSIONS as readonly MetricDimension[]) {
        const rows = [entry('m', { metric_kind: kind, dimension })];
        const plan = metricChartPlan(sel({ metric: 'm' }), rows);
        if (plan.kind !== 'chart') continue;
        expect(plan.query.rate === true && plan.query.agg !== undefined).toBe(false);
        // `perSecond` is the axis label, so it must track the query and not be set independently.
        expect(plan.perSecond).toBe(plan.query.rate === true);
      }
    }
  });

  it('plans a chart for every metric the picker offers', () => {
    // The biconditional that keeps the two halves of the widget in step: if the header offers it,
    // the body must be able to draw it, and vice versa.
    const rows = [
      entry('gauge_none'),
      entry('counter_none', { metric_kind: 'counter' }),
      entry('gauge_entity', { dimension: 'entity' }),
      entry('counter_entity', { dimension: 'entity', metric_kind: 'counter' }),
      entry('gauge_iface', { dimension: 'interface' }),
      entry('counter_iface', { dimension: 'interface', metric_kind: 'counter' }),
    ];
    const offered = new Set(chartableMetrics(rows).map((e) => e.metric));
    for (const r of rows) {
      const plan = metricChartPlan(sel({ metric: r.metric }), rows);
      expect(plan.kind === 'chart').toBe(offered.has(r.metric));
    }
  });
});
