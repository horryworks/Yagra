// SPDX-License-Identifier: AGPL-3.0-only
// Collection tab of the unified node detail: what this node collects, and what it is actually
// producing. A status card (collecting / failing / ICMP-only), the named collection sets it
// inherits from its device profile, every metric it has with the status of each — expandable into a
// chart — and, for admins, the CollectionEditor to add or override node-level metrics.
//
// The metric list is driven by the **inventory** (`listNodeMetrics`), not the collection set, and
// that is the whole point of this screen since ADR-046. The collection set only knows what the node
// was told to collect: it cannot say whether anything arrived, it needs ManageConfig so a viewer saw
// nothing at all, and it has never heard of the metrics that come from check specs rather than OIDs
// — reachability, the URL and DNS monitors, the neighbour count, values extracted from a monitored
// JSON response. Filtering it to `kind === 'scalar'` on top of that meant an operator who added a
// vendor table column could watch it collect successfully and never see a number.
//
// "Sets" map to the templates the node's profile attaches (api.listProfileTemplates); per-set
// interval and per-metric thresholds aren't exposed by the API, so those columns are intentionally
// omitted rather than faked.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { useRangeStore } from '../../store';
import { formatSi, pointsToSeries, scalarDisplay } from '../../lib/format';
import type { CollectionTemplate, NodeDetail, NodeMetricEntry } from '../../types/api';
import { CollectionEditor } from '../CollectionEditor/CollectionEditor';
import { MetricChart } from '../MetricChart/MetricChart';
import { RangeControl, resolveRange, type Range } from './RangeControl';
import { ColumnFilterRow } from '../ui/ColumnFilterRow';
import { ClearFilters } from '../ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { defaultFilters, type FilterState } from '../../lib/columnFilter';
import { facetCounts } from '../../lib/filterCounts';
import { buildPredicate } from '../../lib/filterPredicate';
import { metricColumns } from './tabFilters';
import { viewOf } from '../../lib/metricInventory';
import { fetchNodeMetrics } from '../../lib/metricInventoryCache';
import { agoSec } from '../../lib/format';

type CollState = 'ok' | 'failing' | 'none';

interface Loaded {
  sets: CollectionTemplate[];
  metrics: NodeMetricEntry[];
  profileName: string | null;
  credentialName: string | null;
}

/** One metric's row: name, status, series fan-out, and — when it expands — its chart. */
function MetricRow({
  nodeId,
  entry,
  range,
}: {
  nodeId: string;
  entry: NodeMetricEntry;
  range: Range;
}) {
  const { t } = useTranslation('nodes');
  const [open, setOpen] = useState(false);
  const view = viewOf(entry);
  const expandable = view.chart.kind === 'range' || view.chart.kind === 'rate' || view.chart.kind === 'aggregate';

  const [value, setValue] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [updatedSec, setUpdatedSec] = useState<number | null>(null);

  // The current value loads with the row; the history waits until it is opened. A node with forty
  // metrics would otherwise issue forty range queries the operator never looked at.
  useEffect(() => {
    if (view.read.kind === 'none') return;
    let cancelled = false;
    void api
      .getNodeMetric(nodeId, entry.metric, view.read.kind === 'aggregate' ? { agg: 'max' } : undefined)
      .then((r) => {
        if (!cancelled) setValue(r.value);
      })
      .catch(() => {
        // no reading yet — the status column already says why
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId, entry.metric, view.read.kind]);

  useEffect(() => {
    if (!open || !expandable) return;
    let cancelled = false;
    const { from, to } = resolveRange(range);
    void api
      .getNodeMetricRange(nodeId, entry.metric, {
        from,
        to,
        ...(view.chart.kind === 'aggregate' ? { agg: 'max' as const } : {}),
        ...(view.chart.kind === 'rate' ? { rate: true } : {}),
      })
      .then((r) => {
        if (cancelled) return;
        setSeries(pointsToSeries(r.points));
        setUpdatedSec(r.points.at(-1)?.t ?? null);
      })
      .catch(() => {
        if (!cancelled) setSeries({ timestamps: [], values: [] });
      });
    return () => {
      cancelled = true;
    };
  }, [open, expandable, nodeId, entry.metric, view.chart.kind, range]);

  const display = value == null ? '—' : scalarDisplay(entry.metric, value).value;
  // The per-second suffix is the honest label for a counter charted as a rate — without it the
  // axis reads as the counter itself, which is exactly the confusion `rate=true` exists to remove.
  const yFormat = view.chart.kind === 'rate' ? (v: number) => `${formatSi(v)}/s` : formatSi;

  return (
    <div className="nd-coll-mgroup">
      <div
        className={`nd-coll-mrow${expandable ? ' expandable' : ''}`}
        onClick={expandable ? () => setOpen((o) => !o) : undefined}
        role={expandable ? 'button' : undefined}
        tabIndex={expandable ? 0 : undefined}
        onKeyDown={
          expandable
            ? (e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  setOpen((o) => !o);
                }
              }
            : undefined
        }
      >
        <div className="nd-coll-mcell nd-coll-mname">
          {expandable && <span className={`nd-coll-caret${open ? ' open' : ''}`} aria-hidden />}
          <span className="mono">{entry.metric}</span>
        </div>
        <div className="nd-coll-mcell nd-coll-mshape">
          {t(`collection.dimension.${entry.dimension}`)}
          {entry.series_count > 1 && (
            <span className="nd-coll-mfanout">
              {t('collection.seriesCount', { count: entry.series_count })}
            </span>
          )}
        </div>
        <div className="nd-coll-mcell right nd-coll-mval">{display}</div>
        <div className="nd-coll-mcell right">
          <span className={`nd-coll-mstatus ${entry.status}`}>
            {t(`collection.status.${entry.status}`)}
          </span>
        </div>
      </div>
      {open && expandable && (
        <div className="nd-coll-mchart">
          {series.timestamps.length > 0 ? (
            <>
              <MetricChart
                title=""
                timestamps={series.timestamps}
                values={series.values}
                yFormat={yFormat}
              />
              <div className="nd-coll-mage">{agoSec(updatedSec)}</div>
            </>
          ) : (
            <p className="nd-muted">{t('overview.noHistory')}</p>
          )}
        </div>
      )}
    </div>
  );
}

export function CollectionTab({ node, canEdit }: { node: NodeDetail; canEdit: boolean }) {
  const { t } = useTranslation('nodes');
  const [data, setData] = useState<Loaded | null>(null);
  // Client-side: one node has metrics in the dozens, and the tab already has them all.
  const columns = useMemo(() => metricColumns(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(columns));
  const [sheet, setSheet] = useState(false);
  // The shared window, not a local one: an operator who picks 24h on the Interfaces pane and then
  // opens a metric here expects the same 24 hours (`store.ts` persists it across the panes).
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [sets, profileName, credentialName, metrics] = await Promise.all([
        node.profile_id
          ? api.listProfileTemplates(node.profile_id).catch(() => [] as CollectionTemplate[])
          : Promise.resolve([] as CollectionTemplate[]),
        node.profile_id
          ? api
              .listProfiles()
              .then((ps) => ps.find((p) => p.id === node.profile_id)?.name ?? null)
              .catch(() => null)
          : Promise.resolve(null),
        node.credential_id
          ? api
              .listCredentials()
              .then((cs) => cs.find((c) => c.id === node.credential_id)?.name ?? null)
              .catch(() => null)
          : Promise.resolve(null),
        // Through the shared cache — never stale here (no `maxAgeMs`, so it re-reads), but it is
        // what teaches the dashboard's fleet Top-N the metric names this node has.
        fetchNodeMetrics(node.id),
      ]);
      if (!cancelled) setData({ sets, metrics, profileName, credentialName });
    })();
    return () => {
      cancelled = true;
    };
  }, [node.id, node.profile_id, node.credential_id]);

  const hasSnmp = !!node.credential_id;
  const flowing = data?.metrics.filter((m) => m.status !== 'no_data') ?? [];
  const state: CollState = !hasSnmp ? 'none' : flowing.length > 0 ? 'ok' : 'failing';

  const allMetrics = data?.metrics ?? [];
  const shownMetrics = allMetrics.filter(buildPredicate(columns, filters, Date.now()));
  const metricCounts = Object.fromEntries(
    columns
      .filter((c) => c.filter.kind === 'enum')
      .map((c) => [c.key, facetCounts(allMetrics, columns, filters, c.key, Date.now())]),
  );
  // ⚠️ Keyed by the column each control sits under, and untyped — exactly like `DataTable`'s
  // `specs[c.key]` lookup. Rename a key in `tabFilters.ts` and the cell silently disappears.
  const metricLabels: Record<string, string> = {
    metric: t('collection.colMetric'),
    status: t('collection.colStatus'),
  };

  return (
    <div className="nd-coll">
      <div className={`nd-coll-status ${state}`}>
        <span className={`nd-coll-icon ${state}`} aria-hidden>
          {state === 'ok' ? '✓' : state === 'failing' ? '!' : '○'}
        </span>
        <div>
          <div className="nd-coll-status-t">
            {state === 'ok'
              ? t('collection.statusOk')
              : state === 'failing'
                ? t('collection.statusFailing')
                : t('collection.statusNone')}
          </div>
          <div className="nd-coll-status-s">
            {state === 'ok'
              ? t('collection.setsActive', { count: data?.sets.length ?? 0 })
              : state === 'failing'
                ? t('collection.subFailing')
                : t('collection.subNone')}
          </div>
        </div>
        <div className="nd-coll-status-r">
          <div>
            <div className="nd-coll-stat-k">{t('collection.profile')}</div>
            <div className="nd-coll-stat-v">{data?.profileName ?? '—'}</div>
          </div>
          <div>
            <div className="nd-coll-stat-k">{t('collection.credential')}</div>
            <div className="nd-coll-stat-v">{data?.credentialName ?? '—'}</div>
          </div>
          <div>
            <div className="nd-coll-stat-k">{t('collection.metrics')}</div>
            <div className="nd-coll-stat-v">{data ? data.metrics.length : '—'}</div>
          </div>
        </div>
      </div>

      {data && data.sets.length > 0 && (
        <section>
          <div className="nd-section-t">{t('nav:nodes.metricSets')}</div>
          <div className="nd-coll-sets">
            {data.sets.map((s) => (
              <div className="nd-coll-set" key={s.id}>
                <span className={`nd-coll-set-dot ${state}`} />
                <div>
                  <div className="nd-coll-set-name mono">{s.name}</div>
                  <div className="nd-coll-set-meta">
                    {t('collection.metricCount', { count: s.item_count })}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {data && data.metrics.length > 0 && (
        <section>
          <div className="nd-section-head">
            <div className="nd-section-t">{t('collection.allMetrics')}</div>
            <FilterButton
              columns={columns}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={columns}
              filters={filters}
              onClear={() => setFilters(defaultFilters(columns))}
            />
            <RangeControl value={range} onChange={setRange} />
          </div>
          {sheet && (
            <MobileFilterSheet
              columns={columns}
              labels={metricLabels}
              filters={filters}
              onChange={setFilters}
              counts={metricCounts}
              onClose={() => setSheet(false)}
            />
          )}
          <p className="nd-muted nd-coll-editnote">{t('collection.allMetricsNote')}</p>
          <div className="nd-coll-metrics">
            <div className="nd-coll-mhead">
              <div className="nd-coll-mh">{t('collection.colMetric')}</div>
              <div className="nd-coll-mh">{t('collection.colShape')}</div>
              <div className="nd-coll-mh right">{t('collection.colLastValue')}</div>
              <div className="nd-coll-mh right">{t('collection.colStatus')}</div>
            </div>
            {/* Filter row under the header — same CSS grid rule as the header and every row, so
                the three cannot drift (ADR-053 Inc.6 decision F). Shape and Last value carry empty
                cells: one is derived from the metric name and the other is a live number, and
                neither is a question this list can answer. Not drawn on a phone, where four grid
                tracks in ~366px leave each trigger too narrow to read its own summary —
                `FilterButton` above is the other half, and the gate itself is inside
                `ColumnFilterRow`. */}
            <ColumnFilterRow
              columns={columns}
              slots={['metric', null, null, { key: 'status', align: 'right' }]}
              filters={filters}
              onChange={setFilters}
              counts={metricCounts}
              labels={metricLabels}
              className="nd-coll-mfilters"
            />
            {shownMetrics.map((m) => (
              <MetricRow key={m.metric} nodeId={node.id} entry={m} range={range} />
            ))}
            {shownMetrics.length === 0 && (
              <p className="nd-muted nd-tabpad">{t('common:filter.noMatch')}</p>
            )}
          </div>
        </section>
      )}

      <section>
        <div className="nd-section-t">{t('collection.nodeMetrics')}</div>
        <p className="nd-muted nd-coll-editnote">{t('collection.editNote')}</p>
        <CollectionEditor scope="node" scopeId={node.id} canEdit={canEdit} />
      </section>
    </div>
  );
}
