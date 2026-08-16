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
// Host resources is grouped: one section for core and one per poller pool, with every host in a
// section overlaid on the same charts. It replaced a `<select>` that showed one instance at a time,
// which made the only comparison anyone actually wants — this poller against core, or against its
// pool-mates — a matter of switching and remembering. The grouping and series rules are in
// `hostSections.ts` where they can be tested; what is left here is layout and fetching.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { MetricChart, PALETTE } from '../components/MetricChart/MetricChart';
import { RangeControl, resolveRange } from '../components/NodeDetail/RangeControl';
import type { Range } from '../components/NodeDetail/RangeControl';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { useCan, useRangeStore } from '../store';
import { api, errMsg } from '../services/api';
import { usePolled } from '../dashboard/usePolled';
import { PollerHealthWidget, DataCoverageWidget } from '../dashboard/widgets/monitoring';
import { saveBlob } from '../lib/download';
import { formatBytes, formatUtil } from '../lib/format';
import { diskHeadline } from './diskHeadline';
import { groupHosts, mountPct, peakOf, sectionCharts } from './hostSections';
import type { HostSection } from './hostSections';
import type { DependencyHealth, HostInfo, HostMetricRange } from '../types/api';
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
          <DependencyRow name={t('health.deps.webTls')} dep={data?.web_tls} />
        </ul>
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

/** A multi-host card's headline: the worst current reading in the section, and whose it is.
 *
 *  One number cannot describe several hosts honestly, and "is anything in this pool hot" is the
 *  question a pool section is opened to answer — so the headline names the peak rather than
 *  averaging it away or going blank. Single-host sections keep their own reading instead. */
function peakHeadline(
  hosts: HostInfo[],
  pick: (h: HostInfo) => number | null | undefined,
  format: (v: number) => string,
): string {
  const peak = peakOf(hosts, pick);
  return peak ? `${format(peak.value)} · ${peak.instance}` : '—';
}

/** One section — core, or one pool — with every host in it overlaid on shared charts.
 *
 *  **A collapsed section fetches nothing, and that is the cost control.** `/system/hosts` is one
 *  cheap call driving every section's header and summary, while an expanded section costs one range
 *  request per host every 15s and each of those is six-plus TSDB queries server-side. Bounding the
 *  fan-out by what the operator has open is the only bound that tracks what they are looking at. */
function HostSectionView({ section, range }: { section: HostSection; range: Range }) {
  const { t } = useTranslation('system');
  const [open, setOpen] = useState(true);
  const [ranges, setRanges] = useState<HostMetricRange[]>([]);
  const [win, setWin] = useState<[number, number] | null>(null);

  // A string, not the array: `section.hosts` is rebuilt by `groupHosts` on every 15s inventory
  // refresh, so depending on the array itself would tear down and restart the interval each time.
  const instanceKey = section.hosts.map((h) => h.instance).join('\n');

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const ids = instanceKey.length > 0 ? instanceKey.split('\n') : [];
    const load = () => {
      const { from, to } = resolveRange(range);
      // One request per host, concurrently. A failure resolves to null so a single unreachable
      // poller leaves a gap in its own line rather than blanking the whole section.
      Promise.all(ids.map((id) => api.getHostMetricRange(id, { from, to }).catch(() => null))).then(
        (rs) => {
          if (cancelled) return;
          setRanges(rs.filter((r): r is HostMetricRange => r != null));
          setWin([from, to]);
        },
      );
    };
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [open, instanceKey, range]);

  // Every host keeps its entry even with no trends yet, so its colour stays put — see `hostSections`.
  const entries = useMemo(
    () =>
      section.hosts.map((h) => ({
        label: h.online ? h.instance : `${h.instance}${t('health.offlineSuffix')}`,
        range: ranges.find((r) => r.instance === h.instance) ?? null,
      })),
    [section.hosts, ranges, t],
  );
  const charts = useMemo(() => sectionCharts(entries, PALETTE), [entries]);

  const title =
    section.kind === 'core' ? t('health.core') : t('health.sectionPool', { pool: section.pool });
  const offline = section.hosts.filter((h) => !h.online).length;
  // `null` once there is more than one host — the signal to switch every headline to the peak.
  const only = section.hosts.length === 1 ? section.hosts[0] : null;

  const memHeadline = only
    ? only.mem_used_bytes != null && only.mem_total_bytes != null && only.mem_total_bytes > 0
      ? `${formatBytes(only.mem_used_bytes)} / ${formatBytes(only.mem_total_bytes)}`
      : formatUtil(only.mem_used_pct ?? null)
    : peakHeadline(section.hosts, (h) => h.mem_used_pct, formatUtil);

  return (
    <section className="host-section">
      <div className="host-section-head">
        <button
          type="button"
          className="host-section-toggle"
          aria-expanded={open}
          aria-label={t('health.sectionToggleAria', { section: title })}
          onClick={() => setOpen((v) => !v)}
        >
          <span className="host-section-caret" aria-hidden="true">
            {open ? '▾' : '▸'}
          </span>
          <span className="host-section-title">{title}</span>
        </button>
        {section.kind === 'pool' && (
          <span className="host-section-meta muted">
            {t('health.pollerCount', { count: section.hosts.length })}
            {offline > 0 && ` · ${t('health.offlineCount', { count: offline })}`}
          </span>
        )}
      </div>

      {open ? (
        <div className="host-metrics">
          <HostMetricCard
            label={t('health.metric.cpu')}
            value={
              only
                ? formatUtil(only.cpu_pct ?? null)
                : peakHeadline(section.hosts, (h) => h.cpu_pct, formatUtil)
            }
            timestamps={charts.cpu.timestamps}
            series={charts.cpu.series}
            yFormat={formatUtil}
            yRange={PCT_RANGE}
            win={win}
          />
          <HostMetricCard
            // Two hosts or more and the colour means the host, so 5m/15m give up their two colours.
            label={
              charts.load.multi
                ? t('health.metric.loadAverage1m')
                : t('health.metric.loadAverage')
            }
            value={
              only
                ? only.load1 != null
                  ? only.load1.toFixed(2)
                  : '—'
                : peakHeadline(section.hosts, (h) => h.load1, (v) => v.toFixed(2))
            }
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
              value={
                only
                  ? diskHeadline(only.disks.find((c) => c.mount === d.mount))
                  : peakHeadline(section.hosts, (h) => mountPct(h, d.mount), formatUtil)
              }
              timestamps={d.timestamps}
              series={d.series}
              yFormat={d.known ? formatUtil : formatBytes}
              yRange={d.known ? PCT_RANGE : undefined}
              legendFormat={d.known ? formatUtil : formatBytes}
              win={win}
            />
          ))}
        </div>
      ) : (
        // Collapsed still answers "is anything wrong in here" — from the inventory call the page
        // already makes, so folding a section away costs the charts and nothing else.
        <ul className="host-section-summary">
          {section.hosts.map((h) => (
            <li key={h.instance} className={h.online ? undefined : 'hs-offline'}>
              <span className="hs-name">{h.instance}</span>
              <span className="hs-val muted">
                {t('health.metric.cpu')} <b>{formatUtil(h.cpu_pct ?? null)}</b>
              </span>
              <span className="hs-val muted">
                {t('health.metric.memory')} <b>{formatUtil(h.mem_used_pct ?? null)}</b>
              </span>
              <span className="hs-val muted">
                {t('health.metric.diskShort')} <b>{formatUtil(h.disk_used_pct ?? null)}</b>
              </span>
              {!h.online && <span className="hs-val">{t('health.unreachable')}</span>}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Host resources, as one section for core and one per poller pool, over a shared range picker. */
function HostResourcesCard() {
  const { t } = useTranslation('system');
  const [hosts, setHosts] = useState<HostInfo[]>([]);
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);

  // Instance list (core + pollers reporting telemetry), refreshed on the standard cadence. This is
  // the only unconditional poll here; the per-section trend fetches hang off it.
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

  const sections = useMemo(() => groupHosts(hosts), [hosts]);

  return (
    <div className="host-resources">
      <div className="host-head">
        <span className="host-title">{t('health.hostResources')}</span>
        <div className="host-controls">
          <RangeControl value={range} onChange={setRange} />
        </div>
      </div>
      {sections.length === 0 ? (
        <p className="muted">{t('common:loading')}</p>
      ) : (
        sections.map((s) => <HostSectionView key={s.key} section={s} range={range} />)
      )}
    </div>
  );
}

/** Log windows offered for a support bundle. Bounded by the appender's own retention in practice,
 *  so the longest option is a request rather than a promise — a deployment keeping 48 hourly files
 *  cannot serve a week however this is set. */
const BUNDLE_WINDOWS = [1, 6, 24, 72] as const;

/** Download a support bundle (ADR-045).
 *
 *  Admin-only in the UI because it is Admin-only in the API — the endpoint demands ManageSystem +
 *  ManageCredentials + ViewAudit, so showing the button to an Operator would be offering a 403.
 *  ⚠️ Since ADR-057 an Operator holds one of those three, which is what makes the union guard on
 *  the API side load-bearing rather than decorative.
 *
 *  The failure text is shown verbatim rather than replaced by a generic message: the one refusal
 *  that matters here is the redaction stop, whose message names the file and the rule and is an
 *  instruction to the operator, not a fault to retry past. */
function SupportBundleCard() {
  const { t } = useTranslation('system');
  // The bundle endpoint needs ManageSystem (plus two more); ask for a permission, not a role.
  const canSystem = useCan('manage_system');
  const [hours, setHours] = useState<number>(6);
  // Optional: the node this bundle is *about* (ADR-045 Inc.1). Local state, not the URL — a
  // support bundle is an action taken once, not a view worth sharing a link to.
  const [node, setNode] = useState<{ id: string; name: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ file: string; size: string } | null>(null);

  if (!canSystem) return null;

  const download = () => {
    setBusy(true);
    setError(null);
    setDone(null);
    api
      .downloadSupportBundle(hours, node?.id)
      .then(({ blob, filename }) => {
        // The server names the file; the fallback exists only for a proxy that strips the header.
        const file = filename ?? 'yagra-support.tar.gz';
        saveBlob(blob, file);
        setDone({ file, size: formatBytes(blob.size) });
      })
      .catch((e: unknown) => setError(errMsg(e, t('health.bundle.err'))))
      .finally(() => setBusy(false));
  };

  return (
    <Card title={t('health.cards.supportBundle')} className="support-bundle-card">
      <p className="muted">{t('health.bundle.help')}</p>
      <p className="muted">{t('health.bundle.contents')}</p>
      <div className="sb-actions">
        <label className="sb-window">
          <span className="muted">{t('health.bundle.window')}</span>
          <select
            className="field"
            value={hours}
            onChange={(e) => setHours(Number(e.target.value))}
            disabled={busy}
            aria-label={t('health.bundle.window')}
          >
            {BUNDLE_WINDOWS.map((h) => (
              <option key={h} value={h}>
                {t('health.bundle.windowHours', { count: h })}
              </option>
            ))}
          </select>
        </label>
        <label className="sb-node">
          <span className="muted">{t('health.bundle.node')}</span>
          <NodePicker
            value={node?.id ?? null}
            valueLabel={node?.name}
            onChange={setNode}
            placeholder={t('health.bundle.nodeAny')}
          />
        </label>
        <Button variant="primary" onClick={download} disabled={busy}>
          {busy ? t('health.bundle.busy') : t('health.bundle.action')}
        </Button>
      </div>
      <p className="muted sb-node-help">{t('health.bundle.nodeHelp')}</p>
      {error && <p className="form-error">{error}</p>}
      {done && <p className="sb-done">{t('health.bundle.done', done)}</p>}
    </Card>
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
      {/* Last: it is the thing you reach for once the cards above have failed to explain something. */}
      <SupportBundleCard />
    </div>
  );
}
