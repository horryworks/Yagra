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
import { formatBytes } from '../../lib/format';
import { useRefreshTick } from '../../lib/refreshTick';
import type {
  FlowAsAgg,
  FlowConversation,
  FlowPoint,
  FlowPortAgg,
  FlowProtoAgg,
  FlowTalker,
  NodeDetail,
} from '../../types/api';
import { MetricChart, PALETTE, type ChartSeries } from '../MetricChart/MetricChart';
import { RankedBars, type RankedRow } from '../../dashboard/primitives/RankedBars';
import { DataTable, type Column } from '../ui/DataTable';
import { Select, TextInput } from '../ui/Field';
import { RangeControl, DEFAULT_RANGE, resolveRange, type Range } from './RangeControl';
import { FlowSankey } from './FlowSankey';
import { buildFlowFilters, toggleFilterValue } from './flowFilters';
import './FlowTab.css';

const TOP_N = 10;

/** IP protocol number → short name (the common ones); unknown falls back to `IP <n>`. */
const PROTO_NAMES: Record<number, string> = {
  1: 'ICMP',
  2: 'IGMP',
  6: 'TCP',
  17: 'UDP',
  47: 'GRE',
  50: 'ESP',
  58: 'ICMPv6',
  89: 'OSPF',
  132: 'SCTP',
};
const protoName = (p: number): string => PROTO_NAMES[p] ?? `IP ${p}`;

/** Well-known destination ports → service label; others render as the bare number. */
const PORT_NAMES: Record<number, string> = {
  22: 'SSH',
  25: 'SMTP',
  53: 'DNS',
  80: 'HTTP',
  123: 'NTP',
  161: 'SNMP',
  179: 'BGP',
  443: 'HTTPS',
  514: 'syslog',
  3389: 'RDP',
};
const portLabel = (port: number): string =>
  PORT_NAMES[port] ? `${port} · ${PORT_NAMES[port]}` : String(port);

/** Group flow points by protocol into aligned chart series (top-N protocols by total bytes). */
function buildTrend(points: FlowPoint[]): { timestamps: number[]; series: ChartSeries[] } {
  if (points.length === 0) return { timestamps: [], series: [] };
  const tsSet = new Set<number>();
  const byProto = new Map<number, Map<number, number>>();
  const totals = new Map<number, number>();
  for (const p of points) {
    const ts = Math.floor(p.ts_unix_ms / 1000);
    tsSet.add(ts);
    let m = byProto.get(p.proto);
    if (!m) {
      m = new Map();
      byProto.set(p.proto, m);
    }
    m.set(ts, (m.get(ts) ?? 0) + p.bytes);
    totals.set(p.proto, (totals.get(p.proto) ?? 0) + p.bytes);
  }
  const timestamps = [...tsSet].sort((a, b) => a - b);
  const topProtos = [...totals.entries()]
    .sort((a, b) => b[1] - a[1])
    .slice(0, PALETTE.length)
    .map(([proto]) => proto);
  const series: ChartSeries[] = topProtos.map((proto, i) => {
    const m = byProto.get(proto) ?? new Map<number, number>();
    return {
      label: protoName(proto),
      values: timestamps.map((ts) => m.get(ts) ?? null),
      color: PALETTE[i % PALETTE.length],
    };
  });
  return { timestamps, series };
}

const toRows = <T,>(items: T[], label: (x: T) => string, value: (x: T) => number): RankedRow[] =>
  items.map((x) => ({ label: label(x), value: value(x), valueText: formatBytes(value(x)) }));

export function FlowTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const [range, setRange] = useState<Range>(DEFAULT_RANGE);
  // Filter inputs. `proto`/`asDir` commit immediately (selects); `port`/`peer` are debounced from
  // their raw inputs so typing doesn't refetch on every keystroke.
  const [proto, setProto] = useState('');
  const [asDir, setAsDir] = useState<'src' | 'dst'>('dst');
  const [portInput, setPortInput] = useState('');
  const [peerInput, setPeerInput] = useState('');
  const [port, setPort] = useState('');
  const [peer, setPeer] = useState('');
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

  // Debounce the free-text filters (port/peer) so a fetch fires ~once the operator stops typing.
  useEffect(() => {
    const id = setTimeout(() => {
      setPort(portInput);
      setPeer(peerInput);
    }, 350);
    return () => clearTimeout(id);
  }, [portInput, peerInput]);

  // Click-to-filter: a Top-N row / conversation cell / Sankey node sets the matching filter (and a
  // second click on the already-active value clears it), writing the same state the typed bar drives
  // — including the input mirror, so the bar and the chips stay in sync. Clicks are discrete, so
  // they commit immediately (no debounce).
  const applyPeer = useCallback(
    (addr: string) => {
      const next = toggleFilterValue(peer, addr);
      setPeerInput(next);
      setPeer(next);
    },
    [peer],
  );
  const applyPort = useCallback(
    (p: string) => {
      const next = toggleFilterValue(port, p);
      setPortInput(next);
      setPort(next);
    },
    [port],
  );
  const applyProto = useCallback((p: string) => setProto((cur) => toggleFilterValue(cur, p)), []);
  const clearFilters = useCallback(() => {
    setProto('');
    setPortInput('');
    setPort('');
    setPeerInput('');
    setPeer('');
  }, []);

  useEffect(() => {
    let cancelled = false;
    const { from, to } = resolveRange(range);
    // Build the typed filter set; blank / invalid values are omitted (two or more ⇒ ANDed).
    const filters = buildFlowFilters({ proto, port, peer });

    setLoading(true);
    setError(null);
    void Promise.allSettled([
      api.getNodeFlowSeries(node.id, { from, to, ...filters }),
      api.getNodeFlowTopTalkers(node.id, { from, to, limit: TOP_N, ...filters }),
      api.getNodeFlowConversations(node.id, { from, to, limit: TOP_N, ...filters }),
      api.getNodeFlowTopPorts(node.id, { from, to, limit: TOP_N, ...filters }),
      api.getNodeFlowProtocols(node.id, { from, to, ...filters }),
      api.getNodeFlowTopAs(node.id, { from, to, limit: TOP_N, dir: asDir, ...filters }),
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
  }, [node.id, range, tick, t, proto, port, peer, asDir]);

  const trend = useMemo(() => buildTrend(series), [series]);
  const talkerRows = useMemo(() => toRows(talkers, (x) => x.addr, (x) => x.bytes), [talkers]);
  const protoRows = useMemo(
    () => toRows(protos, (x) => protoName(x.proto), (x) => x.bytes),
    [protos],
  );
  const portRows = useMemo(() => toRows(ports, (x) => portLabel(x.port), (x) => x.bytes), [ports]);
  const asRows = useMemo(
    () =>
      toRows(
        asAgg,
        (x) =>
          x.asn === 0
            ? t('flow.as.unknown')
            : x.name
              ? `AS${x.asn} · ${x.name}`
              : `AS${x.asn}`,
        (x) => x.bytes,
      ),
    [asAgg, t],
  );

  const convoColumns: Column<FlowConversation>[] = useMemo(
    () => [
      {
        key: 'src',
        header: t('flow.col.source'),
        width: '1fr',
        render: (r) => (
          <button type="button" className="nd-flow-ip" onClick={() => applyPeer(r.src)}>
            <span className="mono">{r.src}</span>
          </button>
        ),
      },
      {
        key: 'dst',
        header: t('flow.col.destination'),
        width: '1fr',
        render: (r) => (
          <button type="button" className="nd-flow-ip" onClick={() => applyPeer(r.dst)}>
            <span className="mono">{r.dst}</span>
          </button>
        ),
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

  const filterBar = (
    <div className="nd-flow-filters">
      <Select
        value={proto}
        onChange={(e) => setProto(e.target.value)}
        aria-label={t('flow.filter.proto')}
      >
        <option value="">{t('flow.filter.allProtos')}</option>
        {Object.entries(PROTO_NAMES).map(([n, name]) => (
          <option key={n} value={n}>
            {name}
          </option>
        ))}
      </Select>
      <TextInput
        className="nd-flow-port"
        type="number"
        min={0}
        max={65535}
        placeholder={t('flow.filter.port')}
        value={portInput}
        onChange={(e) => setPortInput(e.target.value)}
        aria-label={t('flow.filter.port')}
      />
      <TextInput
        placeholder={t('flow.filter.peer')}
        value={peerInput}
        onChange={(e) => setPeerInput(e.target.value)}
        aria-label={t('flow.filter.peer')}
      />
    </div>
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

  // The trend reads the proto-only rollup, so port/peer narrow only the tabular + Sankey views.
  const showChartHint = Boolean(port.trim() || peer.trim());

  // Active-filter chips (one per set filter) — click ✕ to remove one, or "Clear filters" for all.
  const chips: { key: string; label: string; mono?: boolean; onRemove: () => void }[] = [];
  if (peer)
    chips.push({
      key: 'peer',
      label: peer,
      mono: true,
      onRemove: () => {
        setPeer('');
        setPeerInput('');
      },
    });
  if (proto)
    chips.push({ key: 'proto', label: protoName(Number(proto)), onRemove: () => setProto('') });
  if (port)
    chips.push({
      key: 'port',
      label: portLabel(Number(port)),
      onRemove: () => {
        setPort('');
        setPortInput('');
      },
    });

  return (
    <div className="nd-flow">
      <div className="nd-flow-toolbar">
        {filterBar}
        <RangeControl value={range} onChange={setRange} />
      </div>
      {chips.length > 0 && (
        <div className="nd-flow-chips">
          <span className="nd-flow-chips-label">{t('flow.filter.active')}</span>
          {chips.map((c) => (
            <span className="nd-flow-chip" key={c.key}>
              <span className={c.mono ? 'mono' : undefined}>{c.label}</span>
              <button
                type="button"
                className="nd-flow-chip-x"
                aria-label={t('flow.filter.remove')}
                onClick={c.onRemove}
              >
                ×
              </button>
            </span>
          ))}
          <button type="button" className="nd-flow-chips-clear" onClick={clearFilters}>
            {t('flow.filter.clear')}
          </button>
        </div>
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
              <RankedBars rows={asRows} empty={t('flow.none')} />
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
