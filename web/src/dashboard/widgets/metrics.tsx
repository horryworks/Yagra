// SPDX-License-Identifier: AGPL-3.0-only
// 03 · The two widgets whose subject the catalog does not know in advance (ADR-046 Inc.2 + Inc.3):
// a chart of any metric of any node, and a fleet ranking by any metric name.
//
// The rest of the catalog answers fixed questions (top CPU, busiest links). These answer the question
// the catalog cannot enumerate: an operator collects `juniper_temp_c` or a value lifted out of a
// monitored JSON body, and wants it in front of them without a curated card existing for it.
//
// The node's metric inventory is what makes the chart possible — it lists what the node actually
// has, including the metrics no collection set contains, and says how each one may be read. Both
// halves of that widget need it (the header to list the choices, the body to know which query to
// issue), so it is fetched once per node and shared through `lib/metricInventoryCache`.
//
// The fleet ranking cannot work that way: there is deliberately no fleet-wide metric inventory to
// read (ADR-046 decision 7), so its field is free text and its suggestions come from the same cache's
// memory of the nodes this session has looked at. All of the judgement — what may be offered, and
// what query each choice implies — lives in `metricChart.ts` and `metricTop.ts`, where tests reach it.

import { useEffect, useState, useSyncExternalStore } from 'react';
import { useTranslation } from 'react-i18next';
import { MetricChart } from '../../components/MetricChart/MetricChart';
import { NodePicker } from '../../components/NodePicker/NodePicker';
import { Select, TextInput } from '../../components/ui/Field';
import { formatSi, pointsToSeries } from '../../lib/format';
import {
  INVENTORY_TTL_MS,
  fetchNodeMetrics,
  metricKindsSnapshot,
  subscribeMetricKinds,
} from '../../lib/metricInventoryCache';
import { api } from '../../services/api';
import type { NodeMetricEntry } from '../../types/api';
import { RankedBars, type RankedRow } from '../primitives/RankedBars';
import type { WidgetProps } from '../types';
import { usePolled } from '../usePolled';
import { chartableMetrics, metricChartPlan, readSelection } from './metricChart';
import { metricSuggestions, metricTopPlan, readTopSelection } from './metricTop';
import { trailingSecs } from './util';

/** Trailing window for the chart (last 6 hours).
 *
 *  Fixed on purpose: the header already carries two pickers, and a third control would crowd the
 *  card at its smallest allowed width. Six hours is the same window the interface heatmap uses —
 *  long enough to show a shape, short enough to stay legible. The node detail owns the adjustable
 *  window, and the metric name in the header is a link's worth of context away from it. */
const SPAN_SECS = 6 * 3600;

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
    // A cached answer is fine here: the inventory changes when a collection set is edited, not on
    // the dashboard's 15s cadence.
    void fetchNodeMetrics(nodeId, INVENTORY_TTL_MS).then((e) => {
      if (!cancelled) setEntries(e);
    });
    return () => {
      cancelled = true;
    };
  }, [nodeId]);
  return entries;
}

/** Customize-mode settings: which node, and which of its metrics.
 *
 *  Both controls choose the *subject*, so neither belongs in the view-mode header (ADR-072). This
 *  widget is the one that ends up with no view-mode actions at all — it has no window and no lens,
 *  only a subject. */
export function MetricChartSettings({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const sel = readSelection(instance.settings);
  const entries = useNodeInventory(sel.nodeId);
  const options = entries ? chartableMetrics(entries) : [];

  return (
    <span className="metricchart-settings">
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

// ── Fleet Top-N by metric (Inc.3) ────────────────────────────────────────────

/** How many nodes the ranking shows. Matches the curated Top-N widgets so the cards look alike. */
const TOP_LIMIT = 6;

/** Every metric name this session has seen, re-rendering the caller when that changes. */
function useKnownMetricKinds() {
  return useSyncExternalStore(subscribeMetricKinds, metricKindsSnapshot);
}

/** Customize-mode settings: which metric to rank by.
 *
 *  The now/1h window that used to sit beside this field is a view control and stays in the card
 *  header — the registry points this widget's `Actions` straight at the shared `TopAggActions`
 *  (ADR-072). */
export function MetricTopSettings({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const known = useKnownMetricKinds();
  const sel = readTopSelection(instance.settings);
  // Typed, not yet committed. The field is committed on blur or Enter rather than per keystroke:
  // every commit persists the layout and re-arms a fleet-wide ranking query, so `hua` and `huaw`
  // would each cost one.
  const [draft, setDraft] = useState(sel.metric ?? '');
  useEffect(() => setDraft(sel.metric ?? ''), [sel.metric]);
  const commit = () => {
    const next = draft.trim();
    if (next !== (sel.metric ?? '')) setSettings({ metric: next === '' ? undefined : next });
  };
  // Datalists are addressed by a document-global id, so it carries the instance's — two of these
  // widgets on one board would otherwise share one list element.
  const listId = `metrictop-names-${instance.instanceId}`;

  return (
    <span className="metrictop-settings">
      <TextInput
        className="mono metrictop-metric"
        list={listId}
        value={draft}
        placeholder={t('widgets.metricTop.metricPlaceholder')}
        aria-label={t('widgets.metricTop.metricAria')}
        title={t('widgets.metricTop.metricAria')}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            commit();
          }
        }}
      />
      <datalist id={listId}>
        {metricSuggestions(known).map((m) => (
          <option key={m} value={m} />
        ))}
      </datalist>
    </span>
  );
}

/** The fleet ranked by one metric, or the reason there is no ranking. */
export function MetricTopWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const known = useKnownMetricKinds();
  const plan = metricTopPlan(readTopSelection(instance.settings), known);

  // Hooks run unconditionally, so the fetch is armed for every plan and asks for nothing when there
  // is nothing to ask for.
  const armed = plan.kind === 'rank' ? plan : null;
  const { data, loading, error } = usePolled(
    () =>
      armed
        ? api.getTopMetrics(armed.metric, { agg: armed.agg, limit: TOP_LIMIT })
        : Promise.resolve(null),
    [armed?.metric, armed?.agg],
  );

  if (plan.kind === 'pick-metric')
    return <p className="muted">{t('widgets.metricTop.pickMetric')}</p>;
  if (plan.kind === 'counter')
    return <p className="muted">{t('widgets.metricTop.counter', { metric: plan.metric })}</p>;

  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows: RankedRow[] = (data?.entries ?? []).map((e) => ({
    label: e.name,
    value: e.value,
    valueText: formatSi(e.value),
  }));
  return (
    <RankedBars
      rows={rows}
      // Naming the metric matters here in a way it does not for a curated widget: an empty ranking
      // has two causes the client cannot tell apart — nothing is reporting it, or the name is not
      // the one the devices use — and the message has to admit both.
      empty={t('widgets.metricTop.empty', { metric: plan.metric })}
      partial={data?.partial}
    />
  );
}
