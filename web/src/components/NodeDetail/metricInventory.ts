// SPDX-License-Identifier: AGPL-3.0-only
// How to read a metric, given what the inventory says it is.
//
// The server answers two facts — is it a gauge or a counter, and what dimension do its series carry
// — and this file turns that pair into the one query the surface may issue. It exists so the choice
// is made in exactly one place, because the wrong choice is not a broken chart: it is a plausible
// one. Plotting a raw counter's stored values draws a smooth rising line that reads as growth, and
// `agg=max` over those values draws the same line while looking like an aggregate. ADR-012 keeps
// counters raw precisely so rates are derived at query time; picking the wrong read here
// reintroduces the accident that decision was made to avoid.
//
// A `.ts` file on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`,
// so judgement left in the `.tsx` is judgement nothing tests.

import type { MetricDimension, MetricKind, NodeMetricEntry } from '../../types/api';

/** How the surface should read one metric's current value. */
export type MetricRead =
  /** `getNodeMetric(node, metric)` — one series per node. */
  | { kind: 'latest' }
  /** `getNodeMetric(node, metric, { agg: 'max' })` — collapse the per-entity rows. */
  | { kind: 'aggregate' }
  /** Nothing to show. A counter's stored value is an odometer reading, not a measurement. */
  | { kind: 'none' };

/** How the surface should chart one metric's history. */
export type MetricChartQuery =
  /** `getNodeMetricRange(node, metric)`. */
  | { kind: 'range' }
  /** `getNodeMetricRange(node, metric, { rate: true })` — a counter as a per-second rate. */
  | { kind: 'rate' }
  /** `getNodeMetricRange(node, metric, { agg: 'max' })` — per-entity gauge as one node series. */
  | { kind: 'aggregate' }
  /** Charted elsewhere: this metric is per-interface and the Interfaces tab owns it. */
  | { kind: 'interfaces' }
  /** Not chartable here, and the surface must say why rather than draw something. */
  | { kind: 'none' };

/** What the surface may do with one inventory entry. */
export interface MetricView {
  read: MetricRead;
  chart: MetricChartQuery;
}

/**
 * The whole decision table, six cells, no defaults.
 *
 * Written out rather than derived from two booleans so every cell is visible at once — including
 * the two that refuse to answer, which are the cells a "reasonable default" would have got wrong.
 */
export function metricView(kind: MetricKind, dimension: MetricDimension): MetricView {
  if (dimension === 'interface') {
    // The per-interface view already exists and shows every row by name. Collapsing these to one
    // node value would answer a different question, worse.
    return {
      read: kind === 'counter' ? { kind: 'none' } : { kind: 'aggregate' },
      chart: { kind: 'interfaces' },
    };
  }
  if (dimension === 'entity') {
    return {
      read: kind === 'counter' ? { kind: 'none' } : { kind: 'aggregate' },
      // A per-entity counter would have to be differentiated per row and then collapsed, and the
      // rows of a folded multi-index table cannot be named in the first place. There is no query
      // for it and the server refuses `rate` with `agg`, so the surface explains instead of drawing.
      chart: kind === 'counter' ? { kind: 'none' } : { kind: 'aggregate' },
    };
  }
  return {
    read: kind === 'counter' ? { kind: 'none' } : { kind: 'latest' },
    chart: kind === 'counter' ? { kind: 'rate' } : { kind: 'range' },
  };
}

/** [`metricView`] for an inventory entry. */
export function viewOf(entry: NodeMetricEntry): MetricView {
  return metricView(entry.metric_kind, entry.dimension);
}

/** Whether an entry has any value or history to show on this surface at all. */
export function isShowable(entry: NodeMetricEntry): boolean {
  const v = viewOf(entry);
  return v.read.kind !== 'none' || (v.chart.kind !== 'none' && v.chart.kind !== 'interfaces');
}

/**
 * The entries the node **Overview** shows as a latest-value strip.
 *
 * Deliberately narrower than the full inventory: the Overview is a glance, and the Collection tab
 * is where everything is reachable. Counters have no glanceable value, and interface-dimensioned
 * metrics have their own tab — including them would put eight octet counters above the fold on
 * every switch.
 */
export function overviewScalars(entries: readonly NodeMetricEntry[]): NodeMetricEntry[] {
  return entries.filter(
    (e) =>
      e.status !== 'no_data' && e.dimension !== 'interface' && viewOf(e).read.kind !== 'none',
  );
}
