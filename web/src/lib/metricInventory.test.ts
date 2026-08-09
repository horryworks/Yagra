// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { isShowable, metricView, overviewScalars, viewOf } from './metricInventory';
import { METRIC_DIMENSIONS, METRIC_KINDS } from '../types/api';
import type { MetricDimension, MetricKind, NodeMetricEntry } from '../types/api';

const entry = (over: Partial<NodeMetricEntry> = {}): NodeMetricEntry => ({
  metric: 'm',
  metric_kind: 'gauge',
  dimension: 'none',
  status: 'ok',
  series_count: 1,
  ...over,
});

describe('metricView', () => {
  it('never charts a counter as its stored values', () => {
    // The failure this function exists to prevent: a raw counter plotted directly draws a smooth
    // rising line that reads as growth, and `agg=max` over one draws the same line while looking
    // like an aggregate. Neither is an error anyone would notice (ADR-012).
    for (const dimension of METRIC_DIMENSIONS) {
      const { read, chart } = metricView('counter', dimension);
      expect(read.kind, dimension).toBe('none');
      expect(chart.kind, dimension).not.toBe('range');
      expect(chart.kind, dimension).not.toBe('aggregate');
    }
  });

  it('charts a node-level counter as a rate and reads no current value for it', () => {
    expect(metricView('counter', 'none')).toEqual({
      read: { kind: 'none' },
      chart: { kind: 'rate' },
    });
  });

  it('reads a node-level gauge directly', () => {
    expect(metricView('gauge', 'none')).toEqual({
      read: { kind: 'latest' },
      chart: { kind: 'range' },
    });
  });

  it('collapses a per-entity gauge to a node aggregate', () => {
    // Row identity is lost at collection time, so a node-wide max is the only honest answer — but
    // it is still an answer, unlike the counter cell below.
    expect(metricView('gauge', 'entity')).toEqual({
      read: { kind: 'aggregate' },
      chart: { kind: 'aggregate' },
    });
  });

  it('refuses to chart a per-entity counter instead of inventing a node-wide rate', () => {
    // There is no query for this: the rows would have to be differentiated individually and then
    // collapsed, and they cannot be named. The server refuses `rate` with `agg` for the same reason.
    expect(metricView('counter', 'entity').chart).toEqual({ kind: 'none' });
  });

  it('hands every interface-dimensioned metric to the Interfaces tab', () => {
    for (const kind of METRIC_KINDS as readonly MetricKind[]) {
      expect(metricView(kind, 'interface').chart).toEqual({ kind: 'interfaces' });
    }
  });

  it('covers every dimension with no fall-through', () => {
    // A dimension added to the backend without a cell here would otherwise land on whichever
    // branch happens to be last.
    for (const dimension of METRIC_DIMENSIONS) {
      for (const kind of METRIC_KINDS as readonly MetricKind[]) {
        const v = metricView(kind, dimension as MetricDimension);
        expect(v.read.kind, `${kind}/${dimension}`).toBeTruthy();
        expect(v.chart.kind, `${kind}/${dimension}`).toBeTruthy();
      }
    }
  });
});

describe('viewOf / isShowable', () => {
  it('reads the pair off the entry', () => {
    expect(viewOf(entry({ metric_kind: 'counter' }))).toEqual(metricView('counter', 'none'));
  });

  it('calls a per-entity counter unshowable but a node-level one showable', () => {
    // The node-level counter has no current value and still has a rate chart, so it is not hidden.
    expect(isShowable(entry({ metric_kind: 'counter' }))).toBe(true);
    expect(isShowable(entry({ metric_kind: 'counter', dimension: 'entity' }))).toBe(false);
  });
});

describe('overviewScalars', () => {
  it('keeps node-level and per-entity gauges and drops the rest', () => {
    const rows = [
      entry({ metric: 'juniper_temp_c' }),
      entry({ metric: 'huawei_cpu_usage', dimension: 'entity' }),
      entry({ metric: 'if_hc_in_octets', metric_kind: 'counter', dimension: 'interface' }),
      entry({ metric: 'ipsec_octets', metric_kind: 'counter' }),
      entry({ metric: 'not_arrived', status: 'no_data', series_count: 0 }),
    ];
    expect(overviewScalars(rows).map((r) => r.metric)).toEqual([
      'juniper_temp_c',
      'huawei_cpu_usage',
    ]);
  });

  it('keeps a metric that has data but no collection item', () => {
    // The case the old Overview could never show: `snmp_neighbor_count` and the URL/DNS monitor
    // metrics come from check specs, so the collection set has never heard of them.
    const rows = [entry({ metric: 'snmp_neighbor_count', status: 'unconfigured' })];
    expect(overviewScalars(rows).map((r) => r.metric)).toEqual(['snmp_neighbor_count']);
  });
});
