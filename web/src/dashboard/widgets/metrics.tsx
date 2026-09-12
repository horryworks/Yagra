// SPDX-License-Identifier: AGPL-3.0-only
// 03 · The widgets built on a node's metric inventory rather than on a fixed question.
//
// Two of them do not know their subject until the operator names it (ADR-046 Inc.2 + Inc.3): a chart
// of any metric of any node, and a fleet ranking by any metric name. The third knows the *question*
// and lets the inventory answer which metric carries it (ADR-136): VPN sessions, where the name
// differs per vendor and nobody should have to know that.
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
import { MetricChart, PALETTE } from '../../components/MetricChart/MetricChart';
import { NodePicker } from '../../components/NodePicker/NodePicker';
import { Button } from '../../components/ui/Button';
import { Select, TextInput } from '../../components/ui/Field';
import { formatCount, formatSi, metricUnitSuffix, pointsToSeries } from '../../lib/format';
import {
  INVENTORY_TTL_MS,
  fetchNodeMetrics,
  metricKindsSnapshot,
  subscribeMetricKinds,
} from '../../lib/metricInventoryCache';
import { api } from '../../services/api';
import type { NodeMetricEntry } from '../../types/api';
import { RankedBars, type RankedRow } from '../primitives/RankedBars';
import type { ViewActionProps, WidgetProps } from '../types';
import { usePolled } from '../usePolled';
import { chartableMetrics, metricChartPlan, readSelection } from './metricChart';
import { metricSuggestions, metricTopPlan, readTopSelection } from './metricTop';
import { WIDGET_RANGES, refreshMsFor, trailingSecs } from './util';
import {
  MAX_VPN_NODES,
  armedKey,
  buildVpnSeries,
  currentReadings,
  everyNodeFailed,
  readVpnSettings,
  vpnSessionsPlan,
  type VpnInventory,
  type VpnNodeRef,
  type VpnNodeSeries,
} from './vpnSessions';

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

// ── VPN sessions (ADR-136) ───────────────────────────────────────────────────

/**
 * The metric inventories of the given nodes, re-fetched only when the set of nodes changes.
 *
 * Deliberately not on the polling tick: a device's inventory changes when someone edits a collection
 * set, not every fifteen seconds. The session history is what polls.
 *
 * The plural twin of {@link useNodeInventory}, and it keeps one thing that one does not: each node
 * is marked per node so `vpnSessionsPlan` can tell "still loading" from "reports none" from "the
 * request failed" — three states it renders three different ways. `Promise.allSettled` is what makes
 * the third reachable; `Promise.all` would turn one unreachable device into six blank cards.
 */
function useNodeInventories(nodeIds: readonly string[]): Record<string, VpnInventory> {
  // The dependency is the joined key, not the array: a fresh array is derived on every render, and
  // passing it would re-fetch every inventory on every keystroke elsewhere in the card.
  const key = nodeIds.join(',');
  const [inv, setInv] = useState<Record<string, VpnInventory>>({});
  useEffect(() => {
    const ids = key === '' ? [] : key.split(',');
    if (ids.length === 0) {
      setInv({});
      return;
    }
    let cancelled = false;
    // Mark every node as loading up front, so a newly added node does not read as "reports none"
    // for the one render before its inventory lands.
    setInv(Object.fromEntries(ids.map((id) => [id, null])));
    void Promise.allSettled(ids.map((id) => fetchNodeMetrics(id, INVENTORY_TTL_MS))).then(
      (results) => {
        if (cancelled) return;
        const next: Record<string, VpnInventory> = {};
        ids.forEach((id, i) => {
          const r = results[i];
          next[id] = r.status === 'fulfilled' ? r.value : 'failed';
        });
        setInv(next);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [key]);
  return inv;
}

/** View-mode header: the time window.
 *
 *  Which devices are plotted is not decided here (ADR-072): adding one changes what the card is
 *  about, so it lives in {@link VpnSessionsSettings} behind the ⚙ the frame draws while the board is
 *  being customized. There is no unit toggle — a session count has one unit, and it is the metric's
 *  own (ADR-136 決定 3), not a lens the operator picks. */
export function VpnSessionsActions({ instance, setSettings }: ViewActionProps) {
  const { t } = useTranslation('dashboard');
  const sel = readVpnSettings(instance.settings);

  return (
    <span className="vpnsess-actions">
      <Select
        value={String(sel.rangeSecs)}
        onChange={(e) => setSettings({ rangeSecs: Number(e.target.value) })}
        aria-label={t('widgets.vpnSessions.rangeAria')}
        title={t('widgets.vpnSessions.rangeAria')}
      >
        {WIDGET_RANGES.map((r) => (
          <option key={r.secs} value={r.secs}>
            {r.label}
          </option>
        ))}
      </Select>
    </span>
  );
}

/** Customize-mode settings: which devices this card plots.
 *
 *  Rendered inside the frame's ⚙ popover, so it owns no trigger and no popover of its own — it is
 *  the panel body. There is no second control beside the node picker, which is the whole point of
 *  ADR-136: the metric is resolved from what the device reports, not chosen. */
export function VpnSessionsSettings({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const sel = readVpnSettings(instance.settings);
  const [pickNode, setPickNode] = useState<{ id: string; name: string } | null>(null);
  const full = sel.nodes.length >= MAX_VPN_NODES;

  const add = () => {
    if (!pickNode || full) return;
    if (sel.nodes.some((n) => n.nodeId === pickNode.id)) return;
    setSettings({
      nodes: [...sel.nodes, { nodeId: pickNode.id, nodeName: pickNode.name } satisfies VpnNodeRef],
    });
    setPickNode(null);
  };

  const remove = (n: VpnNodeRef) =>
    setSettings({ nodes: sel.nodes.filter((x) => x.nodeId !== n.nodeId) });

  const already = pickNode != null && sel.nodes.some((n) => n.nodeId === pickNode.id);

  return (
    <div className="vpnsess-body">
      {/* How many of the six are in use. The cap is otherwise invisible until you hit it — the same
          reason Interface traffic carries this line. */}
      <p className="vpnsess-count">
        {t('widgets.vpnSessions.pickCount', { n: sel.nodes.length, max: MAX_VPN_NODES })}
      </p>
      {sel.nodes.length === 0 ? (
        <p className="muted vpnsess-note">{t('widgets.vpnSessions.noneYet')}</p>
      ) : (
        <ul className="vpnsess-list">
          {sel.nodes.map((n, i) => (
            <li key={n.nodeId} className="vpnsess-item">
              {/* The swatch takes its colour from the same palette index the chart does, so the
                  list and the lines cannot name different colours. */}
              <span
                className="vpnsess-sw"
                style={{ background: PALETTE[i % PALETTE.length] }}
                aria-hidden="true"
              />
              <span className="vpnsess-name">{n.nodeName ?? n.nodeId}</span>
              <button
                type="button"
                className="vpnsess-rm"
                aria-label={t('widgets.vpnSessions.removeAria')}
                title={t('widgets.vpnSessions.removeAria')}
                onClick={() => remove(n)}
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}

      {full ? (
        <p className="muted vpnsess-note">
          {t('widgets.vpnSessions.full', { max: MAX_VPN_NODES })}
        </p>
      ) : (
        <div className="vpnsess-add">
          <NodePicker
            value={pickNode?.id ?? null}
            valueLabel={pickNode?.name}
            placeholder={t('widgets.vpnSessions.pickNodePlaceholder')}
            className="vpnsess-node"
            onChange={setPickNode}
          />
          {/* Add is disabled rather than hidden for a device already on the card: the picker is a
              text search, so "nothing happened" would otherwise be the whole feedback. */}
          {already && <p className="muted vpnsess-note">{t('widgets.vpnSessions.already')}</p>}
          <Button variant="primary" disabled={!pickNode || already} onClick={add}>
            {t('common:actions.add')}
          </Button>
        </div>
      )}
    </div>
  );
}

/** The current session count per device and its history, or the reason there isn't one. */
export function VpnSessionsWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const sel = readVpnSettings(instance.settings);
  const inventory = useNodeInventories(sel.nodes.map((n) => n.nodeId));
  const plan = vpnSessionsPlan(sel, inventory);

  // Hooks run unconditionally, so the fetch is armed for every plan and asks for nothing when there
  // is nothing to ask for.
  const armed = plan.kind === 'chart' && plan.nodes.length > 0 ? plan.nodes : null;
  // `error` is deliberately not destructured: the fetcher below is a `Promise.allSettled`, so it
  // never rejects and this hook can never populate it. See the note at the render branch.
  const { data, loading } = usePolled(
    () => {
      if (!armed) return Promise.resolve(null);
      // One window for every device, resolved once per poll: the series are only comparable if they
      // were asked the same question, and `buildVpnSeries` places values by timestamp on top of
      // that rather than trusting the axes to match.
      const win = trailingSecs(sel.rangeSecs);
      return Promise.allSettled(
        armed.map((n) => api.getNodeMetricRange(n.nodeId, n.metric, { ...win, ...n.query })),
      ).then((results) => ({
        win: [win.from, win.to] as [number, number],
        entries: armed.map((node, i): VpnNodeSeries => {
          const r = results[i];
          return { node, range: r.status === 'fulfilled' ? r.value : null };
        }),
      }));
    },
    [armed ? armedKey(armed) : '', sel.rangeSecs],
    refreshMsFor(sel.rangeSecs),
  );

  if (plan.kind === 'empty') return <p className="muted">{t('widgets.vpnSessions.pickSome')}</p>;
  if (plan.kind === 'loading') return <p className="muted">{t('common:loading')}</p>;

  // Two separate sentences, because they are two separate claims. "Reports no VPN metric" is a fact
  // about the device; "could not be read" is a fact about the request, and saying the first when we
  // learned the second tells the operator their firewall is not a VPN head.
  const notes = (
    <>
      {plan.unsupported.length > 0 && (
        <p className="rankedbars-partial">
          {t('widgets.vpnSessions.unsupported', { nodes: plan.unsupported.join(', ') })}
        </p>
      )}
      {plan.unreadable.length > 0 && (
        <p className="rankedbars-partial">
          {t('widgets.vpnSessions.unreadable', { nodes: plan.unreadable.join(', ') })}
        </p>
      )}
    </>
  );

  if (plan.nodes.length === 0) return notes;
  // ⚠️ No `error` branch, deliberately: the fetcher above is a `Promise.allSettled`, which never
  // rejects, so `usePolled` can only ever hand back `error: null` here. One on this line would read
  // as handled failure — which is how a `401` on every request looked like quiet ports on the
  // sibling widget for the whole of ADR-123 増分 1. What a failure is reported as comes from
  // `everyNodeFailed`, below.
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;

  const entries = data?.entries ?? [];
  const readings = currentReadings(entries, PALETTE);
  const { timestamps, series } = buildVpnSeries(entries, PALETTE);

  return (
    <>
      {notes}
      {/* The numbers. Each carries its own unit noun, which is what makes plotting a Cisco session
          count beside a FortiGate user count honest rather than merely compact (ADR-136 決定 3). */}
      <ul className="vpnsess-chips">
        {readings.map((r) => (
          <li key={r.nodeId} className="vpnsess-chip">
            <span
              className="vpnsess-sw"
              style={{ background: r.color }}
              aria-hidden="true"
            />
            <span className="vpnsess-chip-node">{r.label}</span>
            <span className="vpnsess-chip-val">
              {r.value == null ? '—' : formatCount(r.value)}
            </span>
            {r.value != null && (
              <span className="vpnsess-chip-unit">{metricUnitSuffix(r.metric)}</span>
            )}
          </li>
        ))}
      </ul>
      {timestamps.length === 0 ? (
        <p className="muted">
          {everyNodeFailed(entries)
            ? t('widgets.vpnSessions.seriesFailed')
            : t('widgets.vpnSessions.empty')}
        </p>
      ) : (
        <MetricChart
          title=""
          timestamps={timestamps}
          series={series}
          xRange={data?.win}
          fill
          // The axis is compact (`1.2k`); the cursor readout is the exact count with thousands
          // separators, the same pair `formatCount`'s own doc describes.
          yFormat={formatSi}
          legendFormat={(v) => formatCount(v)}
        />
      )}
    </>
  );
}
