// SPDX-License-Identifier: AGPL-3.0-only
// 03 · Performance hotspots (Top-N). All read fleet Top-N endpoints and render with RankedBars.
// Node Top-N (RTT/CPU/memory) uses /metrics/top (CPU/memory via logical aliases); interface
// Top-N (talkers/errors/busiest) uses /metrics/interface-top. The now/1h-max window is a
// per-instance setting shown in the card's actions slot (shared TopAggActions).

import { useTranslation } from 'react-i18next';
import { Select } from '../../components/ui/Field';
import { formatBps, formatRtt } from '../../lib/format';
import { api } from '../../services/api';
import type { InterfaceTopEntry, InterfaceTopMetric, MetricTopAgg } from '../../types/api';
import { RankedBars, type RankedRow } from '../primitives/RankedBars';
import type { WidgetProps } from '../types';
import { usePolled } from '../usePolled';

/** Read the now/1h window from instance settings (default "now"). */
function aggOf(settings: WidgetProps['instance']['settings']): MetricTopAgg {
  return settings?.agg === 'max_1h' ? 'max_1h' : 'now';
}

/** Shared header action for every Top-N widget: the now / 1h-max window selector. */
export function TopAggActions({ instance, setSettings }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const agg = aggOf(instance.settings);
  return (
    <Select
      value={agg}
      onChange={(e) => setSettings({ agg: e.target.value })}
      aria-label={t('widgets.topAgg.windowAria')}
      title={t('widgets.topAgg.windowTitle')}
    >
      <option value="now">{t('widgets.topAgg.current')}</option>
      <option value="max_1h">{t('widgets.topAgg.peak1h')}</option>
    </Select>
  );
}

/** A node Top-N widget over `/metrics/top` (raw metric or logical `cpu`/`memory`). */
function NodeTopN({
  agg,
  metric,
  format,
  max,
  empty,
}: {
  agg: MetricTopAgg;
  metric: string;
  format: (v: number) => string;
  max?: number;
  empty: string;
}) {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(
    () => api.getTopMetrics(metric, { agg, limit: 6 }),
    [agg, metric],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows: RankedRow[] = (data ?? []).map((e) => ({
    label: e.name,
    value: e.value,
    valueText: format(e.value),
  }));
  return <RankedBars rows={rows} max={max} empty={empty} />;
}

export function TopRttWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  return (
    <NodeTopN
      agg={aggOf(instance.settings)}
      metric="icmp_rtt_ms"
      format={formatRtt}
      empty={t('widgets.topRtt.empty')}
    />
  );
}

const pct = (v: number) => `${Math.round(v)}%`;

export function TopCpuWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  return (
    <NodeTopN
      agg={aggOf(instance.settings)}
      metric="cpu"
      format={pct}
      max={100}
      empty={t('widgets.topCpu.empty')}
    />
  );
}

export function TopMemoryWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  return (
    <NodeTopN
      agg={aggOf(instance.settings)}
      metric="memory"
      format={pct}
      max={100}
      empty={t('widgets.topMemory.empty')}
    />
  );
}

/** A friendly `node · interface` label for an interface Top-N row. */
function ifaceLabel(
  e: Pick<InterfaceTopEntry, 'node_name' | 'if_name' | 'if_alias' | 'ifindex'>,
): string {
  const iface = e.if_name ?? e.if_alias ?? `if${e.ifindex}`;
  return `${e.node_name} · ${iface}`;
}

/** A fleet interface Top-N widget over `/metrics/interface-top`. */
function InterfaceTopN({
  agg,
  metric,
  format,
  empty,
}: {
  agg: MetricTopAgg;
  metric: InterfaceTopMetric;
  format: (v: number) => string;
  empty: string;
}) {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(
    () => api.getInterfaceTop(metric, { agg, limit: 6 }),
    [agg, metric],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const rows: RankedRow[] = (data ?? []).map((e) => ({
    label: ifaceLabel(e),
    value: e.value,
    valueText: format(e.value),
  }));
  return <RankedBars rows={rows} empty={empty} />;
}

export function TopTalkersWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  return (
    <InterfaceTopN
      agg={aggOf(instance.settings)}
      metric="throughput"
      format={formatBps}
      empty={t('widgets.interfaceTraffic.empty')}
    />
  );
}

export function MostErrorsWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  return (
    <InterfaceTopN
      agg={aggOf(instance.settings)}
      metric="errors"
      format={(v) => `${Number(v.toFixed(2))}/s`}
      empty={t('widgets.mostErrors.empty')}
    />
  );
}

/** Busiest links — ranked by throughput, but shown as utilization% where the link speed is known
 *  (falls back to absolute bps for links without a known speed). */
export function BusiestInterfacesWidget({ instance }: WidgetProps) {
  const { t } = useTranslation('dashboard');
  const agg = aggOf(instance.settings);
  const { data, loading, error } = usePolled(
    () => api.getInterfaceTop('throughput', { agg, limit: 8 }),
    [agg],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  const withUtil = (data ?? [])
    .map((e) => {
      // Only compute utilization for a known positive link speed and a non-negative reading;
      // a negative value (corrupt counter math) would yield a nonsensical negative %.
      const util =
        e.if_speed_bps && e.if_speed_bps > 0 && e.value >= 0
          ? (e.value / e.if_speed_bps) * 100
          : null;
      return { e, util };
    })
    .sort((a, b) => (b.util ?? -1) - (a.util ?? -1));
  const rows: RankedRow[] = withUtil.map(({ e, util }) => ({
    label: ifaceLabel(e),
    value: util ?? e.value,
    valueText: util != null ? `${Math.round(util)}% · ${formatBps(e.value)}` : formatBps(e.value),
    color:
      util == null
        ? 'var(--series-1)'
        : util >= 90
          ? 'var(--status-critical)'
          : util >= 70
            ? 'var(--status-warning)'
            : 'var(--series-1)',
  }));
  // util rows use a 0–100 scale; if no speeds are known we fall back to relative bps bars.
  const anyUtil = withUtil.some((w) => w.util != null);
  return (
    <RankedBars
      rows={rows}
      max={anyUtil ? 100 : undefined}
      empty={t('widgets.interfaceTraffic.empty')}
    />
  );
}
