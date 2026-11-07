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
//
// Host resources is one section per host: each core, then each poller under its pool name. It has
// been narrowed twice — a `<select>` showing one instance at a time, then one section per *pool*
// with its pollers overlaid on shared charts. The overlay hid what it was meant to show: two
// pollers reading within a point of each other drew as one line and the headline named only the
// worst, so the page read as a pool-level gauge (ADR-118). The grouping and series rules are in
// `hostSections.ts` where they can be tested; what is left here is layout and fetching.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Badge } from '../components/ui/Badge';
import { InfoTip } from '../components/ui/InfoTip';
import { Facts } from '../components/ui/Facts';
import { Inline, Page, Stack } from '../components/ui/Page';
import { SectionHead } from '../components/ui/Section';
import { MetricChart } from '../components/MetricChart/MetricChart';
import { PALETTE } from '../components/MetricChart/palette';
import { RangeControl, resolveRange } from '../components/NodeDetail/RangeControl';
import type { Range } from '../components/NodeDetail/RangeControl';
import { useRangeStore } from '../store';
import { api } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { PollerHealthWidget, DataCoverageWidget } from '../dashboard/widgets/monitoring';
import { formatBps, formatBytes, formatBytesAxis, formatSi, formatUtil } from '../lib/format';
import type { MirrorAxis } from '../components/MetricChart/mirror';
import { mirrorAxisLabels } from '../dashboard/widgets/interfaceTraffic';
import { diskHeadline } from './diskHeadline';
import { groupHosts, hostCharts } from './hostSections';
import type { HostSection } from './hostSections';
import type { DependencyHealth } from '../types/api';
import './SystemHealthPage.css';

/** A bounded gauge's Y range — CPU/mem/disk read 0–100%, so the baseline is 0. Module-level so
 *  MetricChart isn't rebuilt each refresh. */
const PCT_RANGE: [number, number] = [0, 100];

/** One backing dependency's value: its detail and its reachability badge. */
function DependencyValue({ dep }: { dep: DependencyHealth | undefined }) {
  const { t } = useTranslation('system');
  return (
    <Inline gap={8} justify="between">
      <span className="dep-detail muted">{dep?.detail ?? '—'}</span>
      <Badge tone={dep?.reachable ? 'up' : 'critical'}>
        {dep?.reachable ? t('health.reachable') : t('health.unreachable')}
      </Badge>
    </Inline>
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
        <Facts
          layout="rows"
          items={[
            { label: t('health.deps.postgres'), value: <DependencyValue dep={data?.postgres} /> },
            { label: t('health.deps.tsdb'), value: <DependencyValue dep={data?.tsdb} /> },
            { label: t('health.deps.logs'), value: <DependencyValue dep={data?.logs} /> },
            { label: t('health.deps.flow'), value: <DependencyValue dep={data?.flow} /> },
            { label: t('health.deps.bus'), value: <DependencyValue dep={data?.bus} /> },
            { label: t('health.deps.webTls'), value: <DependencyValue dep={data?.web_tls} /> },
          ]}
        />
      )}
    </Card>
  );
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
  mirrored,
  info,
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
  /** One direction above zero and the other below, with the gutter words (ADR-128). */
  mirrored?: MirrorAxis;
  /** A namespaced `.info` key: an ⓘ beside the label, for what the chart cannot show. */
  info?: string;
  win: [number, number] | null;
}) {
  const { t } = useTranslation('system');
  const hasData = timestamps.length > 0;
  return (
    <Stack gap={8}>
      <Facts
        layout="grid"
        items={[
          {
            key: 'headline',
            label: (
              <>
                {label}
                {info && <InfoTip infoKey={info} label={label} />}
              </>
            ),
            value: <span className="host-metric-value">{value}</span>,
          },
        ]}
      />
      {hasData ? (
        <MetricChart
          title=""
          timestamps={timestamps}
          values={values}
          series={series}
          yFormat={yFormat}
          yRange={yRange}
          legendFormat={legendFormat}
          mirrored={mirrored}
          xRange={win ?? undefined}
        />
      ) : (
        <p className="muted host-metric-empty">{t('health.noHistory')}</p>
      )}
    </Stack>
  );
}

/** One host's section — a core, or one poller under its pool name.
 *
 *  **A collapsed section fetches nothing, and that is the cost control.** `/system/hosts` is one
 *  cheap call driving every section's header and summary, while an expanded one costs a range
 *  request every 15s and each of those is six-plus TSDB queries server-side. Bounding the fan-out
 *  by what the operator has open is the only bound that tracks what they are looking at.
 *
 *  ⚠️ Splitting per host did **not** change that arithmetic: the per-pool version already issued
 *  one request per host. What it costs is the granularity of the toggle — a pool of many pollers is
 *  now many sections to fold away rather than one, which ADR-118 records as deferred, not solved. */
function HostSectionView({ section, range }: { section: HostSection; range: Range }) {
  const { t } = useTranslation('system');
  const [open, setOpen] = useState(true);

  const host = section.host;
  // The id, not the object: `groupHosts` rebuilds `section.host` on every 15s inventory refresh, so
  // depending on the object itself would tear down and restart the interval each time.
  const instance = host.instance;

  // A folded section asks for nothing: its fetch answers `null` without a request, and unfolding
  // it is a dependency change, so the read happens at once.
  const polled = usePolled(
    async () => {
      if (!open) return null;
      const { from, to } = resolveRange(range);
      // A failure resolves to null so an unreachable poller draws empty cards rather than
      // propagating its error to the page.
      const r = await api.getHostMetricRange(instance, { from, to }).catch(() => null);
      return { r, win: [from, to] as [number, number] };
    },
    [open, instance, range],
  );
  const hostRange = polled.data?.r ?? null;
  const win = polled.data?.win ?? null;

  const charts = useMemo(
    () =>
      hostCharts(hostRange, PALETTE, {
        busIn: t('health.series.busIn'),
        busOut: t('health.series.busOut'),
        otherIn: t('health.series.otherIn'),
        otherOut: t('health.series.otherOut'),
      }),
    [hostRange, t],
  );

  const title =
    section.kind === 'core'
      ? section.coreAmbiguous
        ? t('health.coreInstance', { instance })
        : t('health.core')
      : t('health.sectionPoller', { pool: section.pool, instance });

  const memHeadline =
    host.mem_used_bytes != null && host.mem_total_bytes != null && host.mem_total_bytes > 0
      ? `${formatBytes(host.mem_used_bytes)} / ${formatBytes(host.mem_total_bytes)}`
      : formatUtil(host.mem_used_pct ?? null);

  // The heading holds the toggle button rather than being a `Disclosure`: the button carries its own
  // accessible name (`health.sectionToggleAria`), which a `<summary>` has no slot for.
  return (
    <Stack as="section" gap={12} className="host-section">
      <SectionHead
        title={
          <button
            type="button"
            className="host-section-toggle"
            aria-expanded={open}
            aria-label={t('health.sectionToggleAria', { section: title })}
            onClick={() => setOpen((v) => !v)}
          >
            <span aria-hidden="true">{open ? '▾' : '▸'}</span>
            <span>{title}</span>
          </button>
        }
        // The offline marker used to ride on the series label, where the legend named the host.
        // The legend now names 1m/5m/15m, so it has to be said here or not at all.
        actions={!host.online && <span className="host-section-meta">{t('health.unreachable')}</span>}
      />

      {open ? (
        <div className="host-metrics">
          <HostMetricCard
            label={t('health.metric.cpu')}
            value={formatUtil(host.cpu_pct ?? null)}
            timestamps={charts.cpu.timestamps}
            series={charts.cpu.series}
            yFormat={formatUtil}
            yRange={PCT_RANGE}
            win={win}
          />
          <HostMetricCard
            // One host per section ⇒ the colour is free to mean the window again, so all three
            // load averages are drawn rather than 5m/15m giving up their colours to pool-mates.
            label={t('health.metric.loadAverage')}
            value={host.load1 != null ? host.load1.toFixed(2) : '—'}
            timestamps={charts.load.timestamps}
            series={charts.load.series}
            yFormat={(v) => v.toFixed(2)}
            legendFormat={(v) => v.toFixed(2)}
            win={win}
          />
          <HostMetricCard
            label={t('health.metric.memory')}
            value={memHeadline}
            timestamps={charts.mem.timestamps}
            series={charts.mem.series}
            yFormat={formatUtil}
            yRange={PCT_RANGE}
            win={win}
          />
          {charts.disks.map((d) => (
            <HostMetricCard
              key={d.mount}
              label={t('health.metric.disk', { mount: d.mount })}
              value={diskHeadline(host.disks.find((c) => c.mount === d.mount))}
              timestamps={d.timestamps}
              series={d.series}
              // The axis gets the compact form: `214 MB` does not fit the 50 px gutter and loses its
              // leading digit, which reads as a number ten times smaller (ADR-137 decision 10).
              yFormat={d.known ? formatUtil : formatBytesAxis}
              yRange={d.known ? PCT_RANGE : undefined}
              legendFormat={d.known ? formatUtil : formatBytes}
              win={win}
            />
          ))}
          {/* One direction above zero and the other below (ADR-128), the same way up as the
              Interface traffic widget: `mirrorAxisLabels` and the series signs both read its
              `POSITIVE_HALF`. Every reading goes through Math.abs before it is printed, and the axis
              takes the compact form so the gutter cannot clip a leading digit (ADR-137 decision 10). */}
          <HostMetricCard
            label={t('health.metric.network')}
            value={t('health.netHeadline', {
              rx: formatBps(charts.netRate.rx),
              tx: formatBps(charts.netRate.tx),
            })}
            timestamps={charts.net.timestamps}
            series={charts.net.series}
            yFormat={(v) => formatSi(Math.abs(v))}
            legendFormat={(v) => formatBps(Math.abs(v))}
            mirrored={mirrorAxisLabels({ in: t('health.axisIn'), out: t('health.axisOut') })}
            info="system:health.net.info"
            win={win}
          />
          <HostMetricCard
            label={t('health.metric.networkTotal')}
            value={t('health.netHeadline', {
              rx: formatBytes(charts.netSum.rx),
              tx: formatBytes(charts.netSum.tx),
            })}
            timestamps={charts.netTotal.timestamps}
            series={charts.netTotal.series}
            yFormat={(v) => formatBytesAxis(Math.abs(v))}
            legendFormat={(v) => formatBytes(Math.abs(v))}
            mirrored={mirrorAxisLabels({ in: t('health.axisIn'), out: t('health.axisOut') })}
            win={win}
          />
        </div>
      ) : (
        // Collapsed still answers "is anything wrong in here" — from the inventory call the page
        // already makes, so folding a section away costs the charts and nothing else.
        <Inline gap={16} align="baseline" className="host-section-summary">
          <span className={host.online ? 'hs-name' : 'hs-name hs-offline'}>{host.instance}</span>
          <Facts
            layout="inline"
            items={[
              { label: t('health.metric.cpu'), value: <b>{formatUtil(host.cpu_pct ?? null)}</b> },
              {
                label: t('health.metric.memory'),
                value: <b>{formatUtil(host.mem_used_pct ?? null)}</b>,
              },
              {
                label: t('health.metric.diskShort'),
                value: <b>{formatUtil(host.disk_used_pct ?? null)}</b>,
              },
            ]}
          />
          {!host.online && <span>{t('health.unreachable')}</span>}
        </Inline>
      )}
    </Stack>
  );
}

/** Host resources, as one section per host — each core, then each poller under its pool name —
 *  over a shared range picker. */
function HostResourcesCard() {
  const { t } = useTranslation('system');
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);

  // Instance list (core + pollers reporting telemetry), refreshed on the standard cadence. This is
  // the only unconditional poll here; the per-section trend fetches hang off it.
  // A failed read keeps the last list (`usePolled` leaves `data` alone on an error).
  const hosts = usePolled(() => api.getSystemHosts(), []).data?.hosts;
  const sections = useMemo(() => groupHosts(hosts ?? []), [hosts]);

  return (
    <Card
      title={t('health.hostResources')}
      actions={<RangeControl value={range} onChange={setRange} />}
    >
      {sections.length === 0 ? (
        <p className="muted">{t('common:loading')}</p>
      ) : (
        sections.map((s) => <HostSectionView key={s.key} section={s} range={range} />)
      )}
    </Card>
  );
}

export function SystemHealthPage() {
  const { t } = useTranslation('system');
  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:settings.systemHealth')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.systemHealth') }]}
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
      {/* Owns its own Card so the range picker can sit in the header actions slot. */}
      <HostResourcesCard />
    </Page>
  );
}
