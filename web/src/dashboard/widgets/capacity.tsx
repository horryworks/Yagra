// SPDX-License-Identifier: AGPL-3.0-only
// 05 · Capacity & traffic widgets. Traffic spikes/drops rank interfaces by how much their total
// throughput moved vs ~5 min ago (signed delta, bits/sec), rendered with DeltaBars. Delta is a
// series channel (not a node status): spikes use series-4 (amber-brown), drops series-5 (crimson).

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { MetricChart, PALETTE, SERIES_IN, SERIES_OUT } from '../../components/MetricChart/MetricChart';
import { NodePicker } from '../../components/NodePicker/NodePicker';
import { Button } from '../../components/ui/Button';
import { Select } from '../../components/ui/Field';
import { formatBps, formatPps, formatSi } from '../../lib/format';
import { api } from '../../services/api';
import type { InterfaceRow } from '../../types/api';
import { DeltaBars } from '../primitives/DeltaBars';
import { Heatmap } from '../primitives/Heatmap';
import type { ViewActionProps, WidgetProps } from '../types';
import { usePolled } from '../usePolled';
import {
  MAX_LINKS,
  TRAFFIC_RANGES,
  availableInterfaces,
  buildTrafficSeries,
  interfaceLabel,
  interfaceTrafficPlan,
  linkId,
  linksKey,
  readTrafficSettings,
  refreshMsFor,
  selectedNodeIds,
  type LinkRef,
  type LinkSeries,
  type RosterState,
} from './interfaceTraffic';
import { deltaBarRows as toRows, timeColLabels, trailingSecs } from './util';

export function TrafficSpikesWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getInterfaceDelta('up', { limit: 6 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  return (
    <DeltaBars
      rows={toRows(data)}
      color="var(--series-4)"
      empty={t('widgets.trafficSpikes.empty')}
      partial={data?.partial}
    />
  );
}

export function TrafficDropsWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getInterfaceDelta('down', { limit: 6 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  return (
    <DeltaBars
      rows={toRows(data)}
      color="var(--series-5)"
      empty={t('widgets.trafficDrops.empty')}
      partial={data?.partial}
    />
  );
}

// In / Out series colors come from the shared MetricChart palette (single source of truth;
// canvas exemption — uPlot can't read CSS vars).
const IN_COLOR = SERIES_IN;
const OUT_COLOR = SERIES_OUT;

export function AggregateThroughputWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getThroughputRange(), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const ts = data?.timestamps ?? [];
  if (ts.length === 0) return <p className="muted">{t('widgets.throughput.empty')}</p>;
  return (
    <MetricChart
      title=""
      timestamps={ts}
      fill
      yFormat={(v) => formatSi(v)}
      legendFormat={(v) => formatBps(v)}
      series={[
        { label: t('widgets.throughput.in'), values: data?.in_bps ?? [], color: IN_COLOR },
        { label: t('widgets.throughput.out'), values: data?.out_bps ?? [], color: OUT_COLOR },
      ]}
    />
  );
}

export function InterfaceHeatmapWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getInterfaceHeatmap({ limit: 8 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const links = data?.links ?? [];
  const ts = data?.timestamps ?? [];
  if (links.length === 0 || ts.length === 0)
    return <p className="muted">{t('widgets.interfaceTraffic.empty')}</p>;
  return (
    <>
      {data?.partial && (
        <p className="rankedbars-partial">{t('primitives.rankedBars.partial')}</p>
      )}
      <Heatmap
        rowLabels={links}
        colLabels={timeColLabels(ts)}
        values={data?.values ?? []}
        colorBase="var(--series-3)"
        title={(row, col, v) => `${row} ${col || ''} — ${formatBps(v)}`}
      />
    </>
  );
}

// ── Interface traffic — the links the operator named, receive up and transmit down (ADR-069) ──
//
// The counterpart of `aggregate-throughput` (the whole fleet as one line) and of the utilization
// heatmap (whichever links are busiest right now): here the operator says which links, across
// nodes, and each gets a colour with its two directions mirrored about zero.
//
// All judgement is in `interfaceTraffic.ts` — Vitest never executes `.tsx`, so a rule written here
// is a rule nothing tests. What is left in this file is layout and wiring.

/**
 * The interface rosters of the given nodes, re-fetched only when the set of nodes changes.
 *
 * Deliberately not on the polling tick: interface names and speeds change when someone re-cables a
 * rack, not every fifteen seconds. The traffic series is what polls.
 *
 * The rosters are keyed and marked per node so `interfaceTrafficPlan` can tell "still loading" from
 * "the node reports none" from "the request failed" — three states it renders three different ways.
 */
function useInterfaceRosters(nodeIds: readonly string[]): Record<string, RosterState> {
  // The dependency is the joined key, not the array: a fresh array is derived on every render, and
  // passing it would re-fetch every roster on every keystroke elsewhere in the card.
  const key = nodeIds.join(',');
  const [rosters, setRosters] = useState<Record<string, RosterState>>({});
  useEffect(() => {
    const ids = key === '' ? [] : key.split(',');
    if (ids.length === 0) {
      setRosters({});
      return;
    }
    let cancelled = false;
    // Mark every node as loading up front, so a newly added node does not read as "gone" for the
    // one render before its roster lands.
    setRosters(Object.fromEntries(ids.map((id) => [id, null])));
    void Promise.allSettled(ids.map((id) => api.listNodeInterfaces(id))).then((results) => {
      if (cancelled) return;
      const next: Record<string, RosterState> = {};
      ids.forEach((id, i) => {
        const r = results[i];
        next[id] = r.status === 'fulfilled' ? r.value : 'failed';
      });
      setRosters(next);
    });
    return () => {
      cancelled = true;
    };
  }, [key]);
  return rosters;
}

/** View-mode header: how the chosen links are shown — the unit and the time window.
 *
 *  Which links those are is not decided here (ADR-072): adding and removing an interface changes
 *  what the card is about, so it lives in {@link InterfaceTrafficSettings}, behind the ⚙ that the
 *  frame draws while the board is being customized. This was the widget that prompted the rule —
 *  its picker sat in the view-mode header, which on the shared board meant editing the layout
 *  everyone sees without pressing Customize.
 *
 *  `ViewActionProps` rather than `WidgetProps`: the narrowed write is what stops a subject control
 *  from being added back here without the compiler objecting. */
export function InterfaceTrafficActions({ instance, setSettings }: ViewActionProps) {
  const { t } = useTranslation('dashboard');
  const sel = readTrafficSettings(instance.settings);

  return (
    <span className="iftraffic-actions">
      <Select
        value={sel.unit}
        onChange={(e) => setSettings({ unit: e.target.value })}
        aria-label={t('widgets.ifTraffic.unitAria')}
        title={t('widgets.ifTraffic.unitTitle')}
      >
        <option value="bps">{t('widgets.ifTraffic.unitBps')}</option>
        <option value="pps">{t('widgets.ifTraffic.unitPps')}</option>
      </Select>
      <Select
        value={String(sel.rangeSecs)}
        onChange={(e) => setSettings({ rangeSecs: Number(e.target.value) })}
        aria-label={t('widgets.ifTraffic.rangeAria')}
        title={t('widgets.ifTraffic.rangeAria')}
      >
        {TRAFFIC_RANGES.map((r) => (
          <option key={r.secs} value={r.secs}>
            {r.label}
          </option>
        ))}
      </Select>
    </span>
  );
}

/** Customize-mode settings: which interfaces this card plots.
 *
 *  Rendered inside the frame's ⚙ popover, so it owns no trigger and no popover of its own — it is
 *  the panel body. The transient node/ifindex choices stay component state: nothing about a
 *  half-finished pick is worth persisting, and the panel is unmounted the moment Customize ends. */
export function InterfaceTrafficSettings({ instance, setSettings }: WidgetProps) {
  const sel = readTrafficSettings(instance.settings);
  const [pickNode, setPickNode] = useState<{ id: string; name: string } | null>(null);
  const [pickIfindex, setPickIfindex] = useState('');
  const rosters = useInterfaceRosters(pickNode ? [pickNode.id] : []);
  const rows: RosterState = pickNode ? (rosters[pickNode.id] ?? null) : null;

  const full = sel.links.length >= MAX_LINKS;

  const add = () => {
    const ifindex = Number(pickIfindex);
    if (!pickNode || !Number.isInteger(ifindex) || ifindex <= 0 || full) return;
    const row = Array.isArray(rows) ? rows.find((r) => r.ifindex === ifindex) : undefined;
    setSettings({
      links: [
        ...sel.links,
        {
          nodeId: pickNode.id,
          nodeName: pickNode.name,
          ifindex,
          // A snapshot for the list; the body relabels from the live roster.
          ifName: row ? interfaceLabel(row.ifindex, row.if_name, row.if_alias) : null,
        } satisfies LinkRef,
      ],
    });
    setPickIfindex('');
  };

  const remove = (l: LinkRef) =>
    setSettings({ links: sel.links.filter((x) => linkId(x) !== linkId(l)) });

  return (
    <LinkEditor
      links={sel.links}
      rows={rows}
      full={full}
      pickNode={pickNode}
      pickIfindex={pickIfindex}
      onPickNode={(n) => {
        setPickNode(n);
        // The ifindex belongs to the previous node; keeping it would add a link naming one node's
        // id and another's port.
        setPickIfindex('');
      }}
      onPickIfindex={setPickIfindex}
      onAdd={add}
      onRemove={remove}
    />
  );
}

/** The settings panel body: what is plotted, and how to add one more. */
function LinkEditor({
  links,
  rows,
  full,
  pickNode,
  pickIfindex,
  onPickNode,
  onPickIfindex,
  onAdd,
  onRemove,
}: {
  links: LinkRef[];
  rows: RosterState;
  full: boolean;
  pickNode: { id: string; name: string } | null;
  pickIfindex: string;
  onPickNode: (n: { id: string; name: string } | null) => void;
  onPickIfindex: (v: string) => void;
  onAdd: () => void;
  onRemove: (l: LinkRef) => void;
}) {
  const { t } = useTranslation('dashboard');
  const options: InterfaceRow[] =
    Array.isArray(rows) && pickNode ? availableInterfaces(rows, links, pickNode.id) : [];
  const loading = pickNode != null && rows == null;
  const failed = rows === 'failed';

  /** Why the interface select has nothing to offer — the three reasons read differently, and an
   *  unexplained empty list is the case where the operator blames the wrong thing. */
  const placeholder = () => {
    if (!pickNode) return t('widgets.ifTraffic.pickIfacePlaceholder');
    if (loading) return t('common:loading');
    if (failed) return t('widgets.ifTraffic.rosterFailed');
    if (options.length === 0) return t('widgets.ifTraffic.allPicked');
    return t('widgets.ifTraffic.pickIfacePlaceholder');
  };

  return (
    <div className="iftraffic-body">
      {/* How many of the six are in use. The card header carried this before ADR-072 moved the
          picker behind the ⚙; without it the cap is invisible until you hit it. */}
      <p className="iftraffic-count">
        {t('widgets.ifTraffic.pickCount', { n: links.length, max: MAX_LINKS })}
      </p>
      {links.length === 0 ? (
        <p className="muted iftraffic-note">{t('widgets.ifTraffic.noneYet')}</p>
      ) : (
        <ul className="iftraffic-list">
          {links.map((l, i) => (
            <li key={linkId(l)} className="iftraffic-item">
              {/* The swatch takes its colour from the same palette index the chart does, so the
                  list and the lines cannot name different colours. */}
              <span
                className="iftraffic-sw"
                style={{ background: PALETTE[i % PALETTE.length] }}
                aria-hidden="true"
              />
              <span className="iftraffic-name">
                {l.nodeName ? `${l.nodeName} · ` : ''}
                {interfaceLabel(l.ifindex, l.ifName)}
              </span>
              <button
                type="button"
                className="iftraffic-rm"
                aria-label={t('widgets.ifTraffic.removeAria')}
                title={t('widgets.ifTraffic.removeAria')}
                onClick={() => onRemove(l)}
              >
                ✕
              </button>
            </li>
          ))}
        </ul>
      )}

      {full ? (
        <p className="muted iftraffic-note">{t('widgets.ifTraffic.full', { max: MAX_LINKS })}</p>
      ) : (
        <div className="iftraffic-add">
          <NodePicker
            value={pickNode?.id ?? null}
            valueLabel={pickNode?.name}
            placeholder={t('widgets.ifTraffic.pickNodePlaceholder')}
            className="iftraffic-node"
            onChange={onPickNode}
          />
          {/* The interface select and Add share the second row; the node picker has the first to
              itself, because a node name is the longest thing in this panel. */}
          <div className="iftraffic-pick">
            <Select
              value={pickIfindex}
              disabled={options.length === 0}
              aria-label={t('widgets.ifTraffic.pickIfaceAria')}
              title={t('widgets.ifTraffic.pickIfaceAria')}
              onChange={(e) => onPickIfindex(e.target.value)}
            >
              <option value="">{placeholder()}</option>
              {options.map((r) => (
                <option key={r.ifindex} value={r.ifindex}>
                  {interfaceLabel(r.ifindex, r.if_name, r.if_alias)}
                </option>
              ))}
            </Select>
            <Button variant="primary" disabled={!pickIfindex} onClick={onAdd}>
              {t('common:actions.add')}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

/** The mirrored traffic chart for the picked links, or the reason there isn't one. */
export function InterfaceTrafficWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const sel = readTrafficSettings(instance.settings);
  const rosters = useInterfaceRosters(selectedNodeIds(sel.links));
  const plan = interfaceTrafficPlan(sel, rosters);

  // Hooks run unconditionally, so the fetch is armed for every plan and asks for nothing when there
  // is nothing to ask for.
  const armed = plan.kind === 'chart' && plan.links.length > 0 ? plan.links : null;
  const { data, loading, error } = usePolled(
    () => {
      if (!armed) return Promise.resolve(null);
      // One window for every link, resolved once per poll: the series are only comparable if they
      // were asked the same question, and `buildTrafficSeries` places values by timestamp on top of
      // that rather than trusting the axes to match.
      const win = trailingSecs(sel.rangeSecs);
      return Promise.allSettled(
        armed.map((l) => api.getInterfaceSeries(l.nodeId, l.ifindex, win)),
      ).then((results) => ({
        win: [win.from, win.to] as [number, number],
        entries: armed.map((link, i): LinkSeries => {
          const r = results[i];
          return { link, series: r.status === 'fulfilled' ? r.value : null };
        }),
      }));
    },
    // `unit` is deliberately absent: the endpoint returns both units in one response (ADR-060
    // decision 5), so flipping the toggle re-reads arrays already in hand rather than re-querying.
    [armed ? linksKey(armed) : '', sel.rangeSecs],
    refreshMsFor(sel.rangeSecs),
  );

  if (plan.kind === 'empty') return <p className="muted">{t('widgets.ifTraffic.pickSome')}</p>;
  if (plan.kind === 'loading') return <p className="muted">{t('common:loading')}</p>;

  const gone = plan.unavailable.length > 0 && (
    <p className="rankedbars-partial">
      {t('widgets.ifTraffic.unavailable', { links: plan.unavailable.join(', ') })}
    </p>
  );

  if (plan.links.length === 0) return <>{gone}</>;
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;

  const { timestamps, series } = buildTrafficSeries(
    data?.entries ?? [],
    sel.unit,
    PALETTE,
    { in: t('widgets.throughput.in'), out: t('widgets.throughput.out') },
  );
  if (timestamps.length === 0)
    return (
      <>
        {gone}
        <p className="muted">{t('widgets.interfaceTraffic.empty')}</p>
      </>
    );

  // Transmit is plotted below zero, so both axis ticks and the cursor readout report magnitudes —
  // the sign is the direction, not a negative rate.
  const fmt = sel.unit === 'pps' ? formatPps : formatBps;
  return (
    <>
      {gone}
      <MetricChart
        title=""
        timestamps={timestamps}
        series={series}
        xRange={data?.win}
        fill
        yFormat={(v) => formatSi(Math.abs(v))}
        legendFormat={(v) => fmt(Math.abs(v))}
      />
    </>
  );
}
