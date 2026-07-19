// NodeDetail ▸ Flow tab (ADR-031): traffic-flow analysis for this node — a bytes-over-time trend
// (per protocol), Top-N talkers / protocols / destination ports, and a top-conversations table.
// Data comes from the flow-query API (ClickHouse behind core); when flow monitoring isn't enabled
// the endpoints 503 and this renders a "not configured" hint. Reuses the shared RangeControl window
// + refresh tick, MetricChart, RankedBars, and DataTable so it matches every other detail surface.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import { formatBytes } from '../../lib/format';
import { useRefreshTick } from '../../lib/refreshTick';
import type {
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
import { RangeControl, DEFAULT_RANGE, resolveRange, type Range } from './RangeControl';
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
  const [loading, setLoading] = useState(true);
  const [disabled, setDisabled] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [series, setSeries] = useState<FlowPoint[]>([]);
  const [talkers, setTalkers] = useState<FlowTalker[]>([]);
  const [convos, setConvos] = useState<FlowConversation[]>([]);
  const [ports, setPorts] = useState<FlowPortAgg[]>([]);
  const [protos, setProtos] = useState<FlowProtoAgg[]>([]);
  const [xRange, setXRange] = useState<[number, number] | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;
    const { from, to } = resolveRange(range);
    setLoading(true);
    setError(null);
    void Promise.allSettled([
      api.getNodeFlowSeries(node.id, { from, to }),
      api.getNodeFlowTopTalkers(node.id, { from, to, limit: TOP_N }),
      api.getNodeFlowConversations(node.id, { from, to, limit: TOP_N }),
      api.getNodeFlowTopPorts(node.id, { from, to, limit: TOP_N }),
      api.getNodeFlowProtocols(node.id, { from, to }),
    ]).then(([sRes, tRes, cRes, pRes, prRes]) => {
      if (cancelled) return;
      // Flow disabled ⇒ every call 503s with `flow_unavailable`: show the "not configured" state.
      const rejected = [sRes, tRes, cRes, pRes, prRes].find((r) => r.status === 'rejected') as
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
  }, [node.id, range, tick, t]);

  const trend = useMemo(() => buildTrend(series), [series]);
  const talkerRows = useMemo(() => toRows(talkers, (x) => x.addr, (x) => x.bytes), [talkers]);
  const protoRows = useMemo(
    () => toRows(protos, (x) => protoName(x.proto), (x) => x.bytes),
    [protos],
  );
  const portRows = useMemo(() => toRows(ports, (x) => portLabel(x.port), (x) => x.bytes), [ports]);

  const convoColumns: Column<FlowConversation>[] = useMemo(
    () => [
      {
        key: 'src',
        header: t('flow.col.source'),
        width: '1fr',
        render: (r) => <span className="mono">{r.src}</span>,
      },
      {
        key: 'dst',
        header: t('flow.col.destination'),
        width: '1fr',
        render: (r) => <span className="mono">{r.dst}</span>,
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
    [t],
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
    protos.length === 0;

  return (
    <div className="nd-flow">
      <div className="nd-flow-toolbar">
        <RangeControl value={range} onChange={setRange} />
      </div>
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
          </section>

          <div className="nd-flow-grid">
            <section className="nd-flow-card">
              <h3 className="nd-flow-h">{t('flow.topTalkers')}</h3>
              <RankedBars rows={talkerRows} empty={t('flow.none')} />
            </section>
            <section className="nd-flow-card">
              <h3 className="nd-flow-h">{t('flow.topProtocols')}</h3>
              <RankedBars rows={protoRows} empty={t('flow.none')} />
            </section>
            <section className="nd-flow-card">
              <h3 className="nd-flow-h">{t('flow.topPorts')}</h3>
              <RankedBars rows={portRows} empty={t('flow.none')} />
            </section>
          </div>

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
