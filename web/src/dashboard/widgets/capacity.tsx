// 05 · Capacity & traffic widgets. Traffic spikes/drops rank interfaces by how much their total
// throughput moved vs ~5 min ago (signed delta, bits/sec), rendered with DeltaBars. Delta is a
// series channel (not a node status): spikes use series-4 (amber-brown), drops series-5 (crimson).

import { useTranslation } from 'react-i18next';
import { MetricChart, SERIES_IN, SERIES_OUT } from '../../components/MetricChart/MetricChart';
import { formatBps, formatSi } from '../../lib/format';
import { api } from '../../services/api';
import type { InterfaceTopEntry } from '../../types/api';
import { DeltaBars, type DeltaRow } from '../primitives/DeltaBars';
import { Heatmap } from '../primitives/Heatmap';
import { usePolled } from '../usePolled';

/** Sparse HH:MM column labels for a timestamp axis (label ~6 evenly-spaced ticks). */
function timeColLabels(timestamps: number[]): string[] {
  const every = Math.max(1, Math.ceil(timestamps.length / 6));
  return timestamps.map((t, i) =>
    i % every === 0
      ? new Date(t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
      : '',
  );
}

function ifaceLabel(e: InterfaceTopEntry): string {
  const iface = e.if_name ?? e.if_alias ?? `if${e.ifindex}`;
  return `${e.node_name} · ${iface}`;
}

function toRows(data: InterfaceTopEntry[] | null): DeltaRow[] {
  return (data ?? []).map((e) => ({
    label: ifaceLabel(e),
    value: e.value,
    valueText: `${e.value >= 0 ? '+' : '−'}${formatBps(Math.abs(e.value))}`,
  }));
}

export function TrafficSpikesWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getInterfaceDelta('up', { limit: 6 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  return <DeltaBars rows={toRows(data)} color="var(--series-4)" empty={t('widgets.trafficSpikes.empty')} />;
}

export function TrafficDropsWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getInterfaceDelta('down', { limit: 6 }), []);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  return <DeltaBars rows={toRows(data)} color="var(--series-5)" empty={t('widgets.trafficDrops.empty')} />;
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
      height={180}
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
    <Heatmap
      rowLabels={links}
      colLabels={timeColLabels(ts)}
      values={data?.values ?? []}
      colorBase="var(--series-3)"
      title={(row, col, v) => `${row} ${col || ''} — ${formatBps(v)}`}
    />
  );
}
