// SPDX-License-Identifier: AGPL-3.0-only
// 03 · Metric chart — any metric of any node, on the board (ADR-046 Inc.2).
//
// The rest of the catalog answers fixed questions (top CPU, busiest links). This one answers the
// question the catalog cannot enumerate: an operator collects `juniper_temp_c` or a value lifted out
// of a monitored JSON body, and wants it in front of them without a curated card existing for it.
// The node's metric inventory is what makes that possible — it lists what the node actually has,
// including the metrics no collection set contains, and says how each one may be read.
//
// Both halves of the widget need that inventory (the header to list the choices, the body to know
// which query to issue), so it is fetched once per node and shared. All of the judgement — what may
// be offered, and what query each choice implies — lives in `metricChart.ts`, where tests reach it.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { MetricChart } from '../../components/MetricChart/MetricChart';
import { NodePicker } from '../../components/NodePicker/NodePicker';
import { Select } from '../../components/ui/Field';
import { formatSi, pointsToSeries } from '../../lib/format';
import { api } from '../../services/api';
import type { NodeMetricEntry } from '../../types/api';
import type { WidgetProps } from '../types';
import { usePolled } from '../usePolled';
import { chartableMetrics, metricChartPlan, readSelection } from './metricChart';
import { trailingSecs } from './util';

/** Trailing window for the chart (last 6 hours).
 *
 *  Fixed on purpose: the header already carries two pickers, and a third control would crowd the
 *  card at its smallest allowed width. Six hours is the same window the interface heatmap uses —
 *  long enough to show a shape, short enough to stay legible. The node detail owns the adjustable
 *  window, and the metric name in the header is a link's worth of context away from it. */
const SPAN_SECS = 6 * 3600;

/** How long a fetched inventory is reused before being re-fetched (ms).
 *
 *  The inventory changes when someone edits a collection set or a new metric first arrives — not on
 *  the 15s dashboard cadence — so polling it with the chart would be one wasted request per widget
 *  per tick. A few minutes is short enough that a metric added while the board is open turns up
 *  without a reload. */
const INVENTORY_TTL_MS = 180_000;

const cache = new Map<string, { at: number; entries: NodeMetricEntry[] }>();
const inflight = new Map<string, Promise<NodeMetricEntry[]>>();

function fetchInventory(nodeId: string): Promise<NodeMetricEntry[]> {
  const hit = cache.get(nodeId);
  if (hit && Date.now() - hit.at < INVENTORY_TTL_MS) return Promise.resolve(hit.entries);
  const pending = inflight.get(nodeId);
  if (pending) return pending;
  const p = api
    .listNodeMetrics(nodeId)
    .then((entries) => {
      cache.set(nodeId, { at: Date.now(), entries });
      return entries;
    })
    .catch(() => {
      // A node the caller can no longer see, or a store that is down. An empty inventory renders
      // as "not available", which is the same thing the operator needs to know either way.
      return [] as NodeMetricEntry[];
    })
    .finally(() => {
      inflight.delete(nodeId);
    });
  inflight.set(nodeId, p);
  return p;
}

/** The node's metric inventory, or `null` while it is still loading.
 *
 *  `null` and `[]` mean different things downstream (`metricChartPlan` refuses to call a persisted
 *  selection stale until it has actually seen the node's list), so the loading state is not folded
 *  into the empty one. */
function useNodeInventory(nodeId: string | null): NodeMetricEntry[] | null {
  const [entries, setEntries] = useState<NodeMetricEntry[] | null>(null);
  useEffect(() => {
    if (!nodeId) {
      setEntries(null);
      return;
    }
    let cancelled = false;
    setEntries(null);
    void fetchInventory(nodeId).then((e) => {
      if (!cancelled) setEntries(e);
    });
    return () => {
      cancelled = true;
    };
  }, [nodeId]);
  return entries;
}

/** Header actions: which node, and which of its metrics. */
export function MetricChartActions({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const sel = readSelection(instance.settings);
  const entries = useNodeInventory(sel.nodeId);
  const options = entries ? chartableMetrics(entries) : [];

  return (
    <span className="metricchart-actions">
      <NodePicker
        value={sel.nodeId}
        valueLabel={sel.nodeName ?? undefined}
        placeholder={t('widgets.metricChart.pickNodePlaceholder')}
        className="metricchart-node"
        onChange={(n) =>
          // Changing the node invalidates the metric: the same name rarely exists on both, and a
          // silently-kept one would render as "not available" with no hint that it moved.
          setSettings({ nodeId: n?.id, nodeName: n?.name, metric: undefined })
        }
      />
      <Select
        value={sel.metric ?? ''}
        disabled={!sel.nodeId || entries === null}
        onChange={(e) => setSettings({ metric: e.target.value || undefined })}
        aria-label={t('widgets.metricChart.metricAria')}
        title={t('widgets.metricChart.metricAria')}
      >
        <option value="">{t('widgets.metricChart.metricPlaceholder')}</option>
        {/* A persisted metric the node no longer offers still needs an entry, or the select would
            silently snap to the placeholder and hide what the body is complaining about. */}
        {sel.metric && !options.some((o) => o.metric === sel.metric) && (
          <option value={sel.metric}>{sel.metric}</option>
        )}
        {options.map((o) => (
          <option key={o.metric} value={o.metric}>
            {o.metric}
          </option>
        ))}
      </Select>
    </span>
  );
}

/** The chart for one selected node metric, or the reason there isn't one. */
export function MetricChartWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const sel = readSelection(instance.settings);
  const entries = useNodeInventory(sel.nodeId);
  const plan = metricChartPlan(sel, entries);

  // Hooks run unconditionally, so the fetch is armed for every plan and simply asks for nothing
  // when there is nothing to ask for.
  const armed = plan.kind === 'chart' ? plan : null;
  const { data, loading, error } = usePolled(
    () =>
      armed
        ? api.getNodeMetricRange(armed.nodeId, armed.metric, {
            ...trailingSecs(SPAN_SECS),
            ...armed.query,
          })
        : Promise.resolve(null),
    [armed?.nodeId, armed?.metric, armed?.query.agg, armed?.query.rate],
  );

  if (plan.kind === 'pick-node') return <p className="muted">{t('widgets.metricChart.pickNode')}</p>;
  if (plan.kind === 'loading') return <p className="muted">{t('common:loading')}</p>;
  if (plan.kind === 'pick-metric')
    return <p className="muted">{t('widgets.metricChart.pickMetric')}</p>;
  if (plan.kind === 'unavailable')
    return (
      <p className="muted">{t('widgets.metricChart.unavailable', { metric: plan.metric })}</p>
    );

  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const { timestamps, values } = pointsToSeries(data?.points ?? []);
  if (timestamps.length === 0) return <p className="muted">{t('widgets.metricChart.empty')}</p>;
  // The per-second suffix is the honest axis for a counter charted as a rate — without it the axis
  // reads as the counter itself, which is exactly the confusion `rate=true` exists to remove.
  const yFormat = plan.perSecond ? (v: number) => `${formatSi(v)}/s` : formatSi;
  return (
    <MetricChart title="" timestamps={timestamps} values={values} fill yFormat={yFormat} />
  );
}
