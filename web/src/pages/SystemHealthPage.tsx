// SPDX-License-Identifier: AGPL-3.0-only
// System Health (Settings ▸ System Health). Yagra's own health in one place: the poll loop's
// live counters, backing-service reachability (all five stores plus the bus), fleet data coverage,
// and host-resource trends (CPU / load / memory / disk) for the core and every distributed poller.
// Read-only; the backend gates each read with View. Reuses the dashboard monitoring widgets so this
// page and the dashboard cards stay in sync, and the shared MetricChart + RangeControl for the
// trend charts so they match the node-detail health charts.
//
// Split out of the former "Pollers & system health" page so Settings ▸ Pollers can be reserved for
// future distributed-poller configuration.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Badge } from '../components/ui/Badge';
import { MetricChart, PALETTE } from '../components/MetricChart/MetricChart';
import { RangeControl, resolveRange } from '../components/NodeDetail/RangeControl';
import { useRangeStore } from '../store';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { PollerHealthWidget, DataCoverageWidget } from '../dashboard/widgets/monitoring';
import { formatBytes, formatUtil, pointsToSeries } from '../lib/format';
import type {
  DependencyHealth,
  DiskUsage,
  HostInfo,
  HostMetricRange,
  MetricPoint,
} from '../types/api';
import './SystemHealthPage.css';

const REFRESH_MS = 15_000;
/** A bounded gauge's Y range — CPU/mem/disk read 0–100%, so the baseline is 0. Module-level so
 *  MetricChart isn't rebuilt each refresh. */
const PCT_RANGE: [number, number] = [0, 100];

/** One backing dependency as a name + detail + reachability badge. */
function DependencyRow({ name, dep }: { name: string; dep: DependencyHealth | undefined }) {
  const { t } = useTranslation('system');
  return (
    <li className="dep-row">
      <span className="dep-name">{name}</span>
      <span className="dep-detail muted">{dep?.detail ?? '—'}</span>
      <Badge tone={dep?.reachable ? 'up' : 'critical'}>
        {dep?.reachable ? t('health.reachable') : t('health.unreachable')}
      </Badge>
    </li>
  );
}

/** Backing-service reachability: every store the deployment can use, plus the bus. Bus is an
 *  indirect signal inferred from poll-loop activity, and the optional stores (VictoriaLogs,
 *  ClickHouse) report reachable when they are not configured at all — the row detail spells both
 *  out, so an operator can tell "off" from "down".
 *
 *  The header badge is the **server's own** verdict (`overall`), not a re-derivation of the rows.
 *  That is deliberate: this card silently omitted the ClickHouse row for two releases while
 *  `overall` counted it, so a flow-store outage read as "everything reachable". Rendering the
 *  server's answer next to the rows means the next dependency we forget to list disagrees visibly
 *  instead of hiding. */
function DependencyHealthCard() {
  const { t } = useTranslation('system');
  const { data, loading, error } = usePolled(() => api.getSystemHealth(), []);
  const ok = data?.overall === 'ok';
  return (
    <Card
      title={t('health.cards.dependency')}
      actions={
        data && (
          <Badge tone={ok ? 'up' : 'warning'}>
            {ok ? t('health.overallOk') : t('health.overallDegraded')}
          </Badge>
        )
      }
    >
      {error ? (
        <p className="muted">{error}</p>
      ) : loading && !data ? (
        <p className="muted">{t('common:loading')}</p>
      ) : (
        <ul className="dep-list">
          <DependencyRow name={t('health.deps.postgres')} dep={data?.postgres} />
          <DependencyRow name={t('health.deps.tsdb')} dep={data?.tsdb} />
          <DependencyRow name={t('health.deps.logs')} dep={data?.logs} />
          <DependencyRow name={t('health.deps.flow')} dep={data?.flow} />
          <DependencyRow name={t('health.deps.bus')} dep={data?.bus} />
        </ul>
      )}
    </Card>
  );
}

/** Align one series onto a base timestamp axis (missing points → null, drawn as a gap). */
function alignTo(base: number[], points: MetricPoint[]): (number | null)[] {
  const byT = new Map(points.map((p) => [p.t, p.v]));
  return base.map((t) => byT.get(t) ?? null);
}

/** Derive a used-% series by aligning used/size point arrays on shared timestamps. */
function pctSeries(
  used: MetricPoint[],
  size: MetricPoint[],
): { timestamps: number[]; values: number[] } {
  const sizeByT = new Map(size.map((p) => [p.t, p.v]));
  const timestamps: number[] = [];
  const values: number[] = [];
  for (const p of used) {
    const s = sizeByT.get(p.t);
    if (s == null || s <= 0) continue;
    timestamps.push(p.t);
    values.push((p.v / s) * 100);
  }
  return { timestamps, values };
}

/** Human label for an instance in the selector: "Core" or the poller id (with its pool). `t` is
 *  threaded from the caller so the "Core" label follows the active language. */
function instanceLabel(h: HostInfo, t: TFunction): string {
  if (h.role === 'core') return t('health.core');
  return h.pool ? `${h.instance} · ${h.pool}` : h.instance;
}

/** A single metric card: a current-value headline over a trend chart (mirrors the node-detail
 *  Device-health cards). Hides the chart with a muted note until history arrives. */
function HostMetricCard({
  label,
  value,
  timestamps,
  values,
  series,
  yFormat,
  yRange,
  legendFormat,
  win,
}: {
  label: string;
  value: string;
  timestamps: number[];
  values?: number[];
  series?: { label: string; values: (number | null)[]; color?: string }[];
  yFormat?: (v: number) => string;
  yRange?: [number, number];
  legendFormat?: (v: number) => string;
  win: [number, number] | null;
}) {
  const { t } = useTranslation('system');
  const hasData = timestamps.length > 0;
  return (
    <div className="host-metric">
      <div className="host-metric-head">
        <span className="host-metric-label">{label}</span>
        <span className="host-metric-value">{value}</span>
      </div>
      {hasData ? (
        <MetricChart
          title=""
          timestamps={timestamps}
          values={values}
          series={series}
          yFormat={yFormat}
          yRange={yRange}
          legendFormat={legendFormat}
          xRange={win ?? undefined}
        />
      ) : (
        <p className="muted host-metric-empty">{t('health.noHistory')}</p>
      )}
    </div>
  );
}

/** The Disk card headline: "used / size · pct" when capacity is known, else a bare size (the
 *  PostgreSQL `database` proxy has no capacity). */
function diskHeadline(current: DiskUsage | undefined): string {
  if (!current) return '—';
  if (current.size_bytes > 0) {
    const pct = (current.used_bytes / current.size_bytes) * 100;
    return `${formatBytes(current.used_bytes)} / ${formatBytes(current.size_bytes)} · ${formatUtil(pct)}`;
  }
  return formatBytes(current.used_bytes);
}

/** Host resources for the selected instance: an instance selector (Core + each poller) + a shared
 *  range picker, then CPU %, load average, memory %, and a disk card per watched filesystem. */
function HostResourcesCard() {
  const { t } = useTranslation('system');
  const [hosts, setHosts] = useState<HostInfo[]>([]);
  const [instance, setInstance] = useState('core');
  const [data, setData] = useState<HostMetricRange | null>(null);
  const [win, setWin] = useState<[number, number] | null>(null);
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);

  // Instance list (core + pollers reporting telemetry), refreshed on the standard cadence.
  useEffect(() => {
    let cancelled = false;
    const load = () =>
      api
        .getSystemHosts()
        .then((r) => !cancelled && setHosts(r.hosts))
        .catch(() => undefined);
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  // Keep the selection valid if the chosen poller drops out of the fleet.
  useEffect(() => {
    if (hosts.length > 0 && !hosts.some((h) => h.instance === instance)) setInstance('core');
  }, [hosts, instance]);

  // Trend series for the selected instance + window.
  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      api
        .getHostMetricRange(instance, { from, to })
        .then((r) => {
          if (cancelled) return;
          setData(r);
          setWin([from, to]);
        })
        .catch(() => !cancelled && setData(null));
    };
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [instance, range]);

  const current = hosts.find((h) => h.instance === instance) ?? null;

  // Load average as a 3-series chart (1m/5m/15m) on a shared timestamp axis.
  const loadChart = useMemo(() => {
    const base = (data?.load1 ?? []).map((p) => p.t);
    if (base.length === 0) return { timestamps: [] as number[], series: [] };
    return {
      timestamps: base,
      series: [
        { label: '1m', values: alignTo(base, data?.load1 ?? []), color: PALETTE[0] },
        { label: '5m', values: alignTo(base, data?.load5 ?? []), color: PALETTE[1] },
        { label: '15m', values: alignTo(base, data?.load15 ?? []), color: PALETTE[2] },
      ],
    };
  }, [data]);

  const cpu = pointsToSeries(data?.cpu_pct ?? []);
  const mem = pctSeries(data?.mem_used_bytes ?? [], data?.mem_total_bytes ?? []);

  const memHeadline =
    current?.mem_used_bytes != null && current.mem_total_bytes != null && current.mem_total_bytes > 0
      ? `${formatBytes(current.mem_used_bytes)} / ${formatBytes(current.mem_total_bytes)}`
      : formatUtil(current?.mem_used_pct ?? null);

  const options = hosts.length > 0 ? hosts : [];

  return (
    <div className="host-resources">
      <div className="host-head">
        <span className="host-title">{t('health.hostResources')}</span>
        <div className="host-controls">
          <select
            className="host-select"
            value={instance}
            onChange={(e) => setInstance(e.target.value)}
            aria-label={t('health.instanceAria')}
          >
            {options.length === 0 && <option value="core">{t('health.core')}</option>}
            {options.map((h) => (
              <option key={h.instance} value={h.instance}>
                {instanceLabel(h, t)}
                {h.role === 'poller' && !h.online ? t('health.offlineSuffix') : ''}
              </option>
            ))}
          </select>
          <RangeControl value={range} onChange={setRange} />
        </div>
      </div>

      <div className="host-metrics">
        <HostMetricCard
          label={t('health.metric.cpu')}
          value={formatUtil(current?.cpu_pct ?? null)}
          timestamps={cpu.timestamps}
          values={cpu.values}
          yFormat={formatUtil}
          yRange={PCT_RANGE}
          win={win}
        />
        <HostMetricCard
          label={t('health.metric.loadAverage')}
          value={current?.load1 != null ? current.load1.toFixed(2) : '—'}
          timestamps={loadChart.timestamps}
          series={loadChart.series}
          yFormat={(v) => v.toFixed(2)}
          legendFormat={(v) => v.toFixed(2)}
          win={win}
        />
        <HostMetricCard
          label={t('health.metric.memory')}
          value={memHeadline}
          timestamps={mem.timestamps}
          values={mem.values}
          yFormat={formatUtil}
          yRange={PCT_RANGE}
          win={win}
        />
        {(data?.disks ?? []).map((d) => {
          const known = d.size_bytes.some((p) => p.v > 0);
          const dcur = current?.disks.find((c) => c.mount === d.mount);
          const chart = known ? pctSeries(d.used_bytes, d.size_bytes) : pointsToSeries(d.used_bytes);
          return (
            <HostMetricCard
              key={d.mount}
              label={t('health.metric.disk', { mount: d.mount })}
              value={diskHeadline(dcur)}
              timestamps={chart.timestamps}
              values={chart.values}
              yFormat={known ? formatUtil : formatBytes}
              yRange={known ? PCT_RANGE : undefined}
              legendFormat={known ? formatUtil : formatBytes}
              win={win}
            />
          );
        })}
      </div>
    </div>
  );
}

export function SystemHealthPage() {
  const { t } = useTranslation('system');
  return (
    <div>
      <PageHeader
        title={t('nav:settings.systemHealth')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.systemHealth') }]}
        note={t('health.note')}
      />
      <div className="system-health-grid">
        <Card title={t('health.cards.pollLoop')}>
          <PollerHealthWidget />
        </Card>
        {/* Owns its own Card so the server's `overall` verdict can sit in the header actions slot. */}
        <DependencyHealthCard />
        <Card title={t('health.cards.dataCoverage')}>
          <DataCoverageWidget />
        </Card>
      </div>
      <Card className="host-resources-card">
        <HostResourcesCard />
      </Card>
    </div>
  );
}
