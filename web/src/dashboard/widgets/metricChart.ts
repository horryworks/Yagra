// SPDX-License-Identifier: AGPL-3.0-only
// What the "Metric chart" widget should do, given its persisted selection and the selected node's
// metric inventory (ADR-046 Inc.2).
//
// Two things make this worth a file of its own rather than branches inside the `.tsx`. The first is
// that the selection is **persisted and untrusted**: it lives in the widget's opaque settings bag,
// survives across sessions, and names a node and a metric that may since have been deleted, renamed,
// or moved to a device that never had them. A widget that assumes its own settings are still valid
// draws an empty chart and blames the network. The second is that the query shape is not free
// choice — it comes from `metricView`, the same table the node detail uses, so this surface cannot
// invent a reading the rest of the UI refuses to draw (a raw counter charted as its stored values
// is the ADR-012 accident).
//
// A `.ts` on purpose: Vitest runs `environment: 'node'` with `include: ['src/**/*.test.ts']`, so
// judgement left in the `.tsx` is judgement nothing tests.

import { metricView } from '../../lib/metricInventory';
import type { NodeMetricEntry } from '../../types/api';
import type { WidgetSettings } from '../types';

/** The widget's persisted selection. Every field is nullable — a freshly added widget has none. */
export interface MetricChartSelection {
  nodeId: string | null;
  /** The node's name as it was when it was picked, so the trigger has a label without a lookup. */
  nodeName: string | null;
  metric: string | null;
}

/** Read a selection out of the opaque settings bag, ignoring anything that is not a string.
 *
 *  The bag is `Record<string, unknown>` because the layout document is user-editable JSON that has
 *  round-tripped through localStorage and the server; a number where a node id belongs must degrade
 *  to "nothing picked", not to a request for `/nodes/42/metrics`. */
export function readSelection(settings: WidgetSettings | undefined): MetricChartSelection {
  const str = (v: unknown): string | null => (typeof v === 'string' && v !== '' ? v : null);
  return {
    nodeId: str(settings?.nodeId),
    nodeName: str(settings?.nodeName),
    metric: str(settings?.metric),
  };
}

/**
 * The inventory entries this widget can actually draw, in inventory order.
 *
 * Interface-dimensioned metrics are excluded because they are charted per row elsewhere (the node's
 * Interfaces tab, and the interface Top-N widgets) and collapsing eight ports into one line answers
 * a different question, worse. Per-entity counters are excluded because there is no query for them
 * at all — the rate would have to be taken per row and then collapsed, and the rows of a folded
 * multi-index table cannot be named in the first place.
 *
 * A metric with `status: 'no_data'` **is** offered. It was configured and has produced nothing, and
 * the honest rendering of that is an empty chart the operator can leave on the board while they fix
 * the device — not an absence from the picker that reads as "you never configured this".
 */
export function chartableMetrics(entries: readonly NodeMetricEntry[]): NodeMetricEntry[] {
  return entries.filter((e) => {
    const k = metricView(e.metric_kind, e.dimension).chart.kind;
    return k === 'range' || k === 'rate' || k === 'aggregate';
  });
}

/** The range query this widget may issue for one metric. Never both keys: the server refuses
 *  `rate` together with `agg`, because a per-entity counter has no node-level rate. */
export interface MetricChartQueryOpts {
  agg?: 'max';
  rate?: true;
}

/** What the widget body should render this pass. */
export type MetricChartPlan =
  /** Nothing picked yet. */
  | { kind: 'pick-node' }
  /** A node is picked but its inventory has not arrived, so the query shape is not known yet. */
  | { kind: 'loading' }
  /** The node is picked and its inventory is in, but no metric is chosen. */
  | { kind: 'pick-metric' }
  /** The persisted metric is not something this node offers this surface any more. */
  | { kind: 'unavailable'; metric: string }
  /** Draw it. */
  | {
      kind: 'chart';
      nodeId: string;
      metric: string;
      query: MetricChartQueryOpts;
      /** The values are a per-second rate, so the axis must say so. */
      perSecond: boolean;
    };

/**
 * Decide what to render.
 *
 * `entries === null` means the inventory is still loading — distinct from `[]`, which means the
 * node genuinely has no metrics and a persisted selection is therefore stale. Conflating the two
 * would flash "not available" at the operator on every node switch.
 */
export function metricChartPlan(
  sel: MetricChartSelection,
  entries: readonly NodeMetricEntry[] | null,
): MetricChartPlan {
  if (!sel.nodeId) return { kind: 'pick-node' };
  if (entries === null) return { kind: 'loading' };
  if (!sel.metric) return { kind: 'pick-metric' };

  const entry = chartableMetrics(entries).find((e) => e.metric === sel.metric);
  if (!entry) return { kind: 'unavailable', metric: sel.metric };

  const chart = metricView(entry.metric_kind, entry.dimension).chart;
  const query: MetricChartQueryOpts =
    chart.kind === 'rate' ? { rate: true } : chart.kind === 'aggregate' ? { agg: 'max' } : {};
  return {
    kind: 'chart',
    nodeId: sel.nodeId,
    metric: sel.metric,
    query,
    perSecond: chart.kind === 'rate',
  };
}
