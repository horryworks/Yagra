// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Flow tab (ADR-031): traffic-flow analysis for this node — a bytes-over-time trend
// (per protocol), Top-N talkers / protocols / destination ports / autonomous systems, a src→dst
// Sankey, and a conversations table. Drill-down filters (protocol / destination port / peer)
// narrow every fact-table view. Data comes from the flow-query API (ClickHouse behind core); when
// flow monitoring isn't enabled the endpoints 503 and this renders a "not configured" hint. Reuses
// the shared RangeControl window + refresh tick, MetricChart, RankedBars, DataTable, and Field
// controls so it matches every other detail surface.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import { formatBytes, formatAsn } from '../../lib/format';
import { protoName, portLabel } from '../../lib/flowLabels';
import { useRefreshTick } from '../../lib/refreshTick';
import { ClearFilters } from '../ui/ClearFilters';
import { FilterBar } from '../ui/FilterBar';
import { FilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { defaultFilters, type FilterState } from '../../lib/columnFilter';
import {
  chartIgnoresFilters,
  flowFilterColumns,
  flowFilterLabels,
  flowFilters,
  flowQueryFilters,
  toggleFlowValue,
} from './flowTabFilters';
import type {
  FlowAsAgg,
  FlowConversation,
  FlowPoint,
  FlowPortAgg,
  FlowProtoAgg,
  FlowTalker,
  NodeDetail,
} from '../../types/api';
import { MetricChart, PALETTE } from '../MetricChart/MetricChart';
import { RankedBars, type RankedRow } from '../../dashboard/primitives/RankedBars';
import { DataTable, type Column } from '../ui/DataTable';
import { RangeControl, DEFAULT_RANGE, resolveRange, type Range } from './RangeControl';
import { FlowSankey } from './FlowSankey';
// The protocol-series grouping is shared with the fleet-wide throughput widget — one tested
// implementation rather than a second copy that has to be fixed twice.
import { flowTrendSeries } from '../../dashboard/widgets/util';
import './FlowTab.css';

/** How many rows each Top-N and the conversations table hold.
 *
 *  🚨 **This is why the conversations table has no filter row of its own** (ADR-053 決定 M). Those
 *  ten rows are the ten ClickHouse already ranked by bytes; a browser-side predicate over them would
 *  narrow ten out of thousands and then report "no conversations match". Narrowing conversations
 *  means asking the server for a different top-N — which is what the drill-down bar does. */
const TOP_N = 10;

const toRows = <T,>(items: T[], label: (x: T) => string, value: (x: T) => number): RankedRow[] =>
  items.map((x) => ({ label: label(x), value: value(x), valueText: formatBytes(value(x)) }));

export function FlowTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const [range, setRange] = useState<Range>(DEFAULT_RANGE);
  // The four drill-downs, in the shared filter state (ADR-053 Inc.8). Every one of them is a
  // question for the server, so this state's only job is to become query parameters. The debounce
  // that used to live here is inside the `values` cell now, where the other kinds' already were.
  const filterCols = useMemo(() => flowFilterColumns(t), [t]);
  const specs = useMemo(() => flowFilters(t), [t]);
  const labels = useMemo(() => flowFilterLabels(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(filterCols));
  const [sheet, setSheet] = useState(false);
  const [asDir, setAsDir] = useState<'src' | 'dst'>('dst');
  const [loading, setLoading] = useState(true);
  const [disabled, setDisabled] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [series, setSeries] = useState<FlowPoint[]>([]);
  const [talkers, setTalkers] = useState<FlowTalker[]>([]);
  const [convos, setConvos] = useState<FlowConversation[]>([]);
  const [ports, setPorts] = useState<FlowPortAgg[]>([]);
  const [protos, setProtos] = useState<FlowProtoAgg[]>([]);
  const [asAgg, setAsAgg] = useState<FlowAsAgg[]>([]);
  const [xRange, setXRange] = useState<[number, number] | undefined>(undefined);

  // Click-to-filter: a Top-N row / conversation cell / Sankey node **adds** its value to the
  // matching drill-down, and a second click on an already-active value removes it.
  //
  // ⚠️ It used to *replace*, because the filter held one value — so clicking a second talker
  // silently dropped the first, and there was no way to ask "these two hosts". Adding is what the
  // ✕ on each trigger already implied. `toggleFlowValue` normalizes through the column's own parser
  // so a clicked value and a typed one cannot end up as two entries for the same port.
  const toggle = useCallback(
    (key: 'proto' | 'port' | 'peer' | 'asn', clicked: string) =>
      setFilters((cur) => ({
        ...cur,
        [key]: toggleFlowValue(cur[key] ?? '', clicked, specs[key]),
      })),
    [specs],
  );
  const applyPeer = useCallback((addr: string) => toggle('peer', addr), [toggle]);
  const applyPort = useCallback((p: string) => toggle('port', p), [toggle]);
  const applyProto = useCallback((p: string) => toggle('proto', p), [toggle]);
  const applyAsn = useCallback((a: string) => toggle('asn', a), [toggle]);

  // The six queries depend on the joined strings, not on the state object: an object literal has a
  // new identity every render and would refetch the whole fan-out continuously.
  const { proto: qProto, port: qPort, peer: qPeer, asn: qAsn } = flowQueryFilters(filters);

  useEffect(() => {
    let cancelled = false;
    const { from, to } = resolveRange(range);
    // Comma-joined sets; an absent one is omitted, and the endpoint 400s on a value it cannot parse
    // rather than silently returning the unfiltered top-N (ADR-053 決定 Q).
    const f = { proto: qProto, port: qPort, peer: qPeer, asn: qAsn };

    setLoading(true);
    setError(null);
    void Promise.allSettled([
      api.getNodeFlowSeries(node.id, { from, to, ...f }),
      api.getNodeFlowTopTalkers(node.id, { from, to, limit: TOP_N, ...f }),
      api.getNodeFlowConversations(node.id, { from, to, limit: TOP_N, ...f }),
      api.getNodeFlowTopPorts(node.id, { from, to, limit: TOP_N, ...f }),
      api.getNodeFlowProtocols(node.id, { from, to, ...f }),
      api.getNodeFlowTopAs(node.id, { from, to, limit: TOP_N, dir: asDir, ...f }),
    ]).then(([sRes, tRes, cRes, pRes, prRes, aRes]) => {
      if (cancelled) return;
      // Flow disabled ⇒ every call 503s with `flow_unavailable`: show the "not configured" state.
      const results = [sRes, tRes, cRes, pRes, prRes, aRes];
      const rejected = results.find((r) => r.status === 'rejected') as
        | PromiseRejectedResult
        | undefined;
      if (rejected?.reason instanceof ApiError && rejected.reason.code === 'flow_unavailable') {
        setDisabled(true);
        setLoading(false);
        return;
      }
      setDisabled(false);
      if (sRes.status === 'fulfilled') setSeries(sRes.value);
      if (tRes.status === 'fulfilled') setTalkers(tRes.value);
      if (cRes.status === 'fulfilled') setConvos(cRes.value);
      if (pRes.status === 'fulfilled') setPorts(pRes.value);
      if (prRes.status === 'fulfilled') setProtos(prRes.value);
      if (aRes.status === 'fulfilled') setAsAgg(aRes.value);
      setError(
        rejected
          ? rejected.reason instanceof ApiError
            ? rejected.reason.message
            : t('flow.loadError')
          : null,
      );
      setXRange([from, to]);
      setLoading(false);
    });
    return () => {
      cancelled = true;
    };
  }, [node.id, range, tick, t, qProto, qPort, qPeer, qAsn, asDir]);

  const trend = useMemo(() => flowTrendSeries(series, protoName, PALETTE), [series]);
  const talkerRows = useMemo(() => toRows(talkers, (x) => x.addr, (x) => x.bytes), [talkers]);
  const protoRows = useMemo(
    () => toRows(protos, (x) => protoName(x.proto), (x) => x.bytes),
    [protos],
  );
  const portRows = useMemo(() => toRows(ports, (x) => portLabel(x.port), (x) => x.bytes), [ports]);
  const asRows = useMemo(
    () => toRows(asAgg, (x) => formatAsn(x.asn, x.name) ?? t('flow.as.unknown'), (x) => x.bytes),
    [asAgg, t],
  );

  const convoColumns: Column<FlowConversation>[] = useMemo(
    () => [
      {
        key: 'src',
        header: t('flow.col.source'),
        width: '1fr',
        render: (r) => {
          const as = formatAsn(r.src_asn, r.src_as_name);
          return (
            <button type="button" className="nd-flow-ip" onClick={() => applyPeer(r.src)}>
              <span className="mono">{r.src}</span>
              {as && <span className="nd-flow-as">{as}</span>}
            </button>
          );
        },
      },
      {
        key: 'dst',
        header: t('flow.col.destination'),
        width: '1fr',
        render: (r) => {
          const as = formatAsn(r.dst_asn, r.dst_as_name);
          return (
            <button type="button" className="nd-flow-ip" onClick={() => applyPeer(r.dst)}>
              <span className="mono">{r.dst}</span>
              {as && <span className="nd-flow-as">{as}</span>}
            </button>
          );
        },
      },
      {
        key: 'bytes',
        header: t('flow.col.bytes'),
        width: '120px',
        align: 'right',
        render: (r) => formatBytes(r.bytes),
      },
      {
        key: 'packets',
        header: t('flow.col.packets'),
        width: '120px',
        align: 'right',
        render: (r) => r.packets.toLocaleString(),
      },
    ],
    [t, applyPeer],
  );

  if (disabled) {
    return (
      <div className="nd-flow">
        <p className="nd-flow-empty">{t('flow.disabled')}</p>
      </div>
    );
  }

  const isEmpty =
    !loading &&
    series.length === 0 &&
    talkers.length === 0 &&
    convos.length === 0 &&
    ports.length === 0 &&
    protos.length === 0 &&
    asAgg.length === 0;

  // The trend reads the proto-only rollup, so port/peer/AS narrow only the tabular + Sankey views.
  const showChartHint = chartIgnoresFilters(filters);

  return (
    <div className="nd-flow">
      {/* The drill-down governs the whole tab — the chart, all four Top-N cards, the Sankey and the
          conversations table — so it sits above all of them rather than under any one table's
          headers (ADR-053 決定 N). The trigger of each cell is what the hand-rolled chip row used to
          say; the ✕ inside it removes one value, and Clear removes them all. */}
      <div className="nd-flow-toolbar">
        {/* ⚠️ Both of these render `null` most of the time, so they are wrapped: the toolbar is
            `space-between` and needs a left-hand child that always exists, or the range control
            walks to the left edge whenever nothing is filtered. */}
        <div className="nd-flow-toolbar-actions">
          <FilterButton
            columns={filterCols}
            filters={filters}
            onOpen={() => setSheet(true)}
          />
          <ClearFilters
            columns={filterCols}
            filters={filters}
            onClear={() => setFilters(defaultFilters(filterCols))}
          />
        </div>
        <RangeControl value={range} onChange={setRange} />
      </div>
      <FilterBar
        columns={filterCols}
        labels={labels}
        filters={filters}
        onChange={setFilters}
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          labels={labels}
          filters={filters}
          onChange={setFilters}
          onClose={() => setSheet(false)}
        />
      )}
      {error && <p className="nd-flow-error">{error}</p>}

      {isEmpty ? (
        <p className="nd-flow-empty">{t('flow.empty')}</p>
      ) : (
        <>
          <section className="nd-flow-trend">
            {trend.timestamps.length > 0 ? (
              <MetricChart
                title={t('flow.trend')}
                timestamps={trend.timestamps}
                series={trend.series}
                xRange={xRange}
                height={220}
                yFormat={formatBytes}
                legendFormat={formatBytes}
              />
            ) : (
              <p className="nd-flow-muted">{t('flow.noTrend')}</p>
            )}
            {showChartHint && <p className="nd-flow-muted">{t('flow.filter.chartHint')}</p>}
          </section>

          <div className="nd-flow-grid">
            <section className="nd-flow-card">
              <h3 className="nd-flow-h">{t('flow.topTalkers')}</h3>
              <RankedBars
                rows={talkerRows}
                empty={t('flow.none')}
                onRowClick={(i) => applyPeer(talkers[i].addr)}
              />
            </section>
            <section className="nd-flow-card">
              <h3 className="nd-flow-h">{t('flow.topProtocols')}</h3>
              <RankedBars
                rows={protoRows}
                empty={t('flow.none')}
                onRowClick={(i) => applyProto(String(protos[i].proto))}
              />
            </section>
            <section className="nd-flow-card">
              <h3 className="nd-flow-h">{t('flow.topPorts')}</h3>
              <RankedBars
                rows={portRows}
                empty={t('flow.none')}
                onRowClick={(i) => applyPort(String(ports[i].port))}
              />
            </section>
            <section className="nd-flow-card">
              <div className="nd-flow-card-head">
                <h3 className="nd-flow-h">{t('flow.topAs')}</h3>
                <div className="rc-seg" role="group" aria-label={t('flow.as.direction')}>
                  <button
                    type="button"
                    className={`rc-seg-btn${asDir === 'dst' ? ' active' : ''}`}
                    aria-pressed={asDir === 'dst'}
                    onClick={() => setAsDir('dst')}
                  >
                    {t('flow.as.dstDir')}
                  </button>
                  <button
                    type="button"
                    className={`rc-seg-btn${asDir === 'src' ? ' active' : ''}`}
                    aria-pressed={asDir === 'src'}
                    onClick={() => setAsDir('src')}
                  >
                    {t('flow.as.srcDir')}
                  </button>
                </div>
              </div>
              <RankedBars
                rows={asRows}
                empty={t('flow.none')}
                onRowClick={(i) => applyAsn(String(asAgg[i].asn))}
              />
            </section>
          </div>

          <section className="nd-flow-sankey">
            <h3 className="nd-flow-h">{t('flow.sankey')}</h3>
            {convos.length > 0 ? (
              <FlowSankey conversations={convos} onNodeClick={applyPeer} />
            ) : (
              <p className="nd-flow-muted">{t('flow.none')}</p>
            )}
          </section>

          <section className="nd-flow-convos">
            <h3 className="nd-flow-h">{t('flow.conversations')}</h3>
            <div className="nd-flow-table">
              <DataTable
                rows={convos}
                columns={convoColumns}
                rowKey={(r) => `${r.src}->${r.dst}`}
                empty={t('flow.none')}
                loading={loading}
              />
            </div>
          </section>
        </>
      )}
    </div>
  );
}
