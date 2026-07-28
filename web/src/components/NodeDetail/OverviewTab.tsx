// SPDX-License-Identifier: AGPL-3.0-only
// Overview tab of the unified node detail. The compact core (per the redesign) is an "ICMP RTT ·
// last 30 min" sparkline + a two-column facts grid. Below it sit the richer, relocated sections —
// Active alerts, Device health (CPU/Mem), System (SNMP) scalars — each of which self-hides when the
// node has no such data, so a simple ICMP-only node shows just sparkline + facts, while a fully
// monitored device shows everything. Nothing from the old detail page is dropped, only restyled.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import {
  deriveMem,
  formatBytes,
  formatCount,
  formatDaysToExpiry,
  formatRtt,
  formatSi,
  formatTimestamp,
  formatUptimeTicks,
  formatUtil,
  httpStatusLabel,
  httpStatusTone,
  pointsToSeries,
  scalarDisplay,
  severityColorVar,
  stateLabel,
} from '../../lib/format';
import { groupPath } from '../../lib/nodeTree';
import { poolFactLabel, polledByIsWarning, polledByLabel } from '../../lib/pool';
import type {
  MetricPoint,
  NodeAssignment,
  NodeDetail,
  NodeGroup,
  NodeState,
  NodeStatus,
  NodeSummary,
} from '../../types/api';
import { MetricChart } from '../MetricChart/MetricChart';
import { RangeControl, resolveRange, type Range } from './RangeControl';
import { DnsHealth } from './DnsHealth';
import { useRangeStore } from '../../store';
import { useRefreshTick } from '../../lib/refreshTick';

interface Props {
  node: NodeDetail;
  groups: NodeGroup[];
  nodes?: NodeSummary[];
  status: NodeStatus | null;
  /** RTT history (last ~30 min), shared with the header's "seen" line. */
  series: { timestamps: number[]; values: number[] };
  unreachable: boolean;
}

export function OverviewTab({ node, groups, nodes, status, series, unreachable }: Props) {
  const { t } = useTranslation('nodes');
  const facts = useFacts(node, groups, nodes, unreachable);
  return (
    <div className="nd-overview">
      <section>
        <div className="nd-section-t">{t('overview.icmpRttTitle')}</div>
        {series.timestamps.length > 0 ? (
          <MetricChart
            title=""
            timestamps={series.timestamps}
            values={series.values}
            height={96}
            yFormat={(v) => `${Math.round(v)}`}
            legendFormat={formatRtt}
          />
        ) : (
          <p className="nd-muted nd-spark-empty">
            {unreachable ? t('overview.unreachableDash') : t('overview.noRttHistory')}
          </p>
        )}
      </section>

      <div className="nd-facts">
        {facts.map((f) => (
          <div key={f.label}>
            <div className="nd-fact-k">{f.label}</div>
            <div className={`nd-fact-v${f.mono ? ' mono' : ''}${f.warn ? ' warn' : ''}`}>
              {f.value}
            </div>
          </div>
        ))}
      </div>

      {status && status.alerts.length > 0 && (
        <section>
          <div className="nd-section-t">{t('nav:alerts.active')}</div>
          <div className="nd-alerts">
            {status.alerts.map((a) => (
              <div className="nd-alert" key={`${a.check}|${a.severity}`}>
                <span
                  className="nd-alert-dot"
                  style={{ background: severityColorVar(a.severity) }}
                />
                <span className="nd-alert-state">{stateLabel(a.state)}</span>
                {a.root_cause && (
                  <span className="nd-muted mono nd-alert-cause">
                    {t('overview.causedBy', { cause: a.root_cause })}
                  </span>
                )}
                {a.flapping && <span className="nd-alert-flap">{t('alerts:row.flapping')}</span>}
                <span className="nd-muted nd-alert-time">{formatTimestamp(a.at_unix_ms)}</span>
              </div>
            ))}
          </div>
        </section>
      )}

      {/* Gated on `node.kind` — the kind the backend actually polls — not on which config happens
          to be non-null. A node can carry more than one of these rows (older ones predate the API
          guard that now refuses a second), and only one of them is ever probed; keying off the
          configs rendered a health card for every row, so a Meraki node could show a URL-monitor
          card whose metrics never arrive. The config check that follows is type narrowing. */}
      {node.kind === 'url' && node.url_check && (
        <UrlHealth nodeId={node.id} url={node.url_check.url} />
      )}
      {node.kind === 'dns' && node.dns_check && (
        <DnsHealth nodeId={node.id} check={node.dns_check} />
      )}
      {node.kind === 'meraki' && node.meraki_device && (
        <MerakiHealth nodeId={node.id} device={node.meraki_device} />
      )}
      <DeviceHealth nodeId={node.id} />
      <SnmpScalars nodeId={node.id} />
    </div>
  );
}

/** CSS status-palette variable for an HTTP tone (up/warning/critical). */
function httpToneVar(tone: 'up' | 'warning' | 'critical'): string {
  if (tone === 'up') return 'var(--status-ok)';
  if (tone === 'warning') return 'var(--status-warning)';
  return 'var(--status-critical)';
}

/** Tone for a TLS cert's days-to-expiry against the built-in URL thresholds (warn < 30, crit < 7). */
function certTone(days: number): 'up' | 'warning' | 'critical' {
  if (days < 7) return 'critical';
  if (days < 30) return 'warning';
  return 'up';
}

/** URL-monitor health: availability (`http_up`), the last HTTP status code, and (for HTTPS) the
 *  TLS certificate's days-to-expiry. Mirrors the Device-health card layout; self-refreshes. Shown
 *  only for URL-monitor nodes (the caller guards on `node.url_check`). */
function UrlHealth({ nodeId, url }: { nodeId: string; url: string }) {
  const { t } = useTranslation('nodes');
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);
  const tick = useRefreshTick();
  const [up, setUp] = useState<number | null>(null);
  const [statusCode, setStatusCode] = useState<number | null>(null);
  const [certDays, setCertDays] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [win, setWin] = useState<[number, number] | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      void Promise.allSettled([
        api.getNodeMetric(nodeId, 'http_up'),
        api.getNodeMetric(nodeId, 'http_status_code'),
        api.getNodeMetric(nodeId, 'ssl_cert_days_to_expiry'),
        api.getNodeMetricRange(nodeId, 'http_status_code', { from, to }),
      ]).then(([u, s, c, r]) => {
        if (cancelled) return;
        setUp(u.status === 'fulfilled' ? u.value.value : null);
        setStatusCode(s.status === 'fulfilled' ? s.value.value : null);
        setCertDays(c.status === 'fulfilled' ? c.value.value : null);
        setSeries(
          r.status === 'fulfilled' ? pointsToSeries(r.value.points) : { timestamps: [], values: [] },
        );
        setWin([from, to]);
      });
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [nodeId, range, tick]);

  const code = statusCode == null ? null : Math.round(statusCode);
  const availabilityTone: 'up' | 'critical' = up === 1 ? 'up' : 'critical';

  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">{t('overview.urlMonitor')}</div>
        <RangeControl value={range} onChange={setRange} />
      </div>
      <div className="nd-fact-v mono nd-url-target">{url}</div>
      <div className="nd-health-metrics">
        <div className="nd-health-metric">
          <div className="nd-health-metric-head">
            <span className="nd-health-metric-label">{t('overview.availability')}</span>
            <span
              className="nd-health-metric-value"
              style={{ color: httpToneVar(availabilityTone) }}
            >
              {up == null ? '—' : up === 1 ? t('overview.up') : t('overview.down')}
            </span>
          </div>
          <div className="nd-url-status-line">
            {code == null ? (
              <span className="nd-muted">{t('overview.noResponse')}</span>
            ) : (
              <span style={{ color: httpToneVar(httpStatusTone(code)) }}>
                HTTP {code} · {httpStatusLabel(code)}
              </span>
            )}
          </div>
          {series.timestamps.length > 0 && (
            <MetricChart
              title=""
              timestamps={series.timestamps}
              values={series.values}
              yFormat={(v) => `${Math.round(v)}`}
              legendFormat={(v) => `HTTP ${Math.round(v)}`}
              xRange={win ?? undefined}
            />
          )}
        </div>
        {certDays != null && (
          <div className="nd-health-metric">
            <div className="nd-health-metric-head">
              <span className="nd-health-metric-label">{t('overview.tlsCertificate')}</span>
              <span
                className="nd-health-metric-value"
                style={{ color: httpToneVar(certTone(certDays)) }}
              >
                {formatDaysToExpiry(certDays)}
              </span>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}

/** Human-friendly kilobyte formatter for Meraki windowed usage gauges. */
function formatKb(kb: number): string {
  if (kb >= 1_048_576) return `${(kb / 1_048_576).toFixed(1)} GB`;
  if (kb >= 1024) return `${(kb / 1024).toFixed(1)} MB`;
  return `${Math.round(kb)} KB`;
}

/** Cisco Meraki device health: availability (`meraki_device_up`), client count, and windowed
 *  traffic usage — all node-level gauges from the org collector. Shown only for Meraki nodes
 *  (the caller guards on `node.meraki_device`). Per-uplink loss/latency live on the Interfaces tab. */
function MerakiHealth({
  nodeId,
  device,
}: {
  nodeId: string;
  device: NonNullable<NodeDetail['meraki_device']>;
}) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const [up, setUp] = useState<number | null>(null);
  const [clients, setClients] = useState<number | null>(null);
  const [sent, setSent] = useState<number | null>(null);
  const [recv, setRecv] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      void Promise.allSettled([
        api.getNodeMetric(nodeId, 'meraki_device_up'),
        api.getNodeMetric(nodeId, 'meraki_client_count'),
        api.getNodeMetric(nodeId, 'meraki_usage_sent_kb'),
        api.getNodeMetric(nodeId, 'meraki_usage_recv_kb'),
      ]).then(([u, c, s, r]) => {
        if (cancelled) return;
        setUp(u.status === 'fulfilled' ? u.value.value : null);
        setClients(c.status === 'fulfilled' ? c.value.value : null);
        setSent(s.status === 'fulfilled' ? s.value.value : null);
        setRecv(r.status === 'fulfilled' ? r.value.value : null);
      });
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [nodeId, tick]);

  const availabilityTone: 'up' | 'critical' = up === 1 ? 'up' : 'critical';

  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">Cisco Meraki</div>
      </div>
      <div className="nd-fact-v mono nd-url-target">
        {t('overview.merakiIdentity', { productType: device.product_type, serial: device.serial })}
      </div>
      <div className="nd-health-metrics">
        <div className="nd-health-metric">
          <div className="nd-health-metric-head">
            <span className="nd-health-metric-label">{t('overview.availability')}</span>
            <span
              className="nd-health-metric-value"
              style={{ color: httpToneVar(availabilityTone) }}
            >
              {up == null ? '—' : up === 1 ? t('overview.online') : t('overview.offline')}
            </span>
          </div>
        </div>
        <div className="nd-health-metric">
          <div className="nd-health-metric-head">
            <span className="nd-health-metric-label">{t('overview.clients')}</span>
            <span className="nd-health-metric-value">
              {clients == null ? '—' : Math.round(clients)}
            </span>
          </div>
        </div>
        <div className="nd-health-metric">
          <div className="nd-health-metric-head">
            <span className="nd-health-metric-label">{t('overview.trafficSentRecv')}</span>
            <span className="nd-health-metric-value">
              {sent == null && recv == null
                ? '—'
                : `${sent == null ? '—' : formatKb(sent)} / ${recv == null ? '—' : formatKb(recv)}`}
            </span>
          </div>
        </div>
      </div>
    </section>
  );
}

interface Fact {
  label: string;
  value: string;
  mono?: boolean;
  /** Render the value as a warning (paired with text, never colour alone). */
  warn?: boolean;
}

/** Resolve the facts-grid rows for a node: group breadcrumb, poll-pool + current poller, address,
 *  maker/model, the profile/credential names (looked up by id), the parent-node name, and uptime. */
function useFacts(
  node: NodeDetail,
  groups: NodeGroup[],
  nodes: NodeSummary[] | undefined,
  unreachable: boolean,
): Fact[] {
  const { t } = useTranslation('nodes');
  const [profileName, setProfileName] = useState<string | null>(null);
  const [credentialName, setCredentialName] = useState<string | null>(null);
  const [parentName, setParentName] = useState<string | null>(null);
  const [uptime, setUptime] = useState<string | null>(null);
  const [assignment, setAssignment] = useState<NodeAssignment | undefined>(undefined);

  useEffect(() => {
    let cancelled = false;
    if (node.profile_id) {
      api
        .listProfiles()
        .then((ps) => !cancelled && setProfileName(ps.find((p) => p.id === node.profile_id)?.name ?? null))
        .catch(() => undefined);
    } else setProfileName(null);
    if (node.credential_id) {
      api
        .listCredentials()
        .then(
          (cs) =>
            !cancelled && setCredentialName(cs.find((c) => c.id === node.credential_id)?.name ?? null),
        )
        .catch(() => undefined);
    } else setCredentialName(null);
    return () => {
      cancelled = true;
    };
  }, [node.profile_id, node.credential_id]);

  // Parent node name: from the already-loaded inventory list if present, else a targeted fetch.
  useEffect(() => {
    let cancelled = false;
    if (!node.parent_id) {
      setParentName(null);
      return;
    }
    const known = nodes?.find((n) => n.id === node.parent_id)?.name;
    if (known) {
      setParentName(known);
      return;
    }
    api
      .getNode(node.parent_id)
      .then((n) => !cancelled && setParentName(n.name))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [node.parent_id, nodes]);

  // Uptime from the SNMP sysUpTime scalar (TimeTicks).
  useEffect(() => {
    let cancelled = false;
    setUptime(null);
    api
      .getNodeMetric(node.id, 'snmp_sys_uptime_ticks')
      .then((r) => !cancelled && setUptime(formatUptimeTicks(r.value)))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [node.id]);

  // Effective pool + current poller (ADR-009/020). Live coordinator state, so it is its own read
  // rather than part of the node row; an older core without the endpoint just leaves it undefined
  // and both facts render an em dash.
  useEffect(() => {
    let cancelled = false;
    setAssignment(undefined);
    api
      .getNodeAssignment(node.id)
      .then((a) => !cancelled && setAssignment(a))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [node.id]);

  const path = groupPath(groups, node.group_id);
  const groupName = (id: string) => groups.find((g) => g.id === id)?.name;
  return [
    { label: t('field.group'), value: path.length ? path.join(' / ') : t('ungrouped') },
    // Placement facts sit together: which folder, which pool, which poller.
    { label: t('field.pool'), value: poolFactLabel(assignment, groupName, t), mono: true },
    {
      label: t('field.polledBy'),
      value: polledByLabel(assignment?.polled_by, t),
      mono: assignment?.polled_by.state === 'assigned',
      warn: polledByIsWarning(assignment?.polled_by),
    },
    { label: t('field.ipAddress'), value: node.address, mono: true },
    { label: t('field.maker'), value: node.vendor || '—' },
    { label: t('field.model'), value: node.model || '—', mono: true },
    { label: t('field.deviceProfile'), value: profileName ?? '—' },
    { label: t('field.snmpCredential'), value: credentialName ?? '—' },
    { label: t('field.parentNode'), value: parentName ?? '—', mono: !!parentName },
    {
      label: t('field.uptime'),
      value: unreachable ? t('overview.unreachableDash') : (uptime ?? '—'),
    },
  ];
}

/** ifOperStatus (1 = up) → a node-state colour for status dots/charts. (Shared with the
 *  Interfaces tab via a re-export.) */
export function operState(oper: number | null): NodeState {
  if (oper == null) return 'unknown';
  return oper === 1 ? 'ok' : 'critical';
}

/** A bounded gauge's Y range — CPU/Mem read as 0–100%, so the chart baseline is 0, not the
 *  window's min. Module-level + stable so MetricChart isn't rebuilt on every refresh. */
const PCT_RANGE: [number, number] = [0, 100];

/** A real device's total RAM is never a handful of bytes — below this floor a "total memory"
 *  reading is bogus (e.g. a vendor size OID unsupported on the device, or one whose value
 *  overflows a 32-bit SNMP INTEGER so the node-wide `max()` lands on a tiny wrong row). When
 *  the total isn't trustworthy we show the % only rather than a wrong size like "90 B". */
const MIN_MEM_TOTAL_BYTES = 1024 * 1024;

/** CPU% candidates (vendor/host gauges that read 0–100); the first one the node collects wins. */
const CPU_METRICS = ['huawei_cpu_usage', 'cisco_cpu_5min', 'hr_processor_load'];

/** Firewall session-count gauges (current concurrent sessions); the first one the node collects
 *  wins. Mixed sources: Huawei USG / Cisco ASA are walked as per-entity tables (collapsed node-wide
 *  via query-time max(), like CPU); Fortinet is a scalar. */
const SESSION_TOTAL_METRICS = [
  'huawei_usg_total_sessions',
  'fortinet_sessions',
  'asa_current_connections',
  'panos_sessions_active',
];

/** New-session setup-rate gauges (sessions/sec). Separate card from the total because the scale is
 *  utterly different (a count vs a per-second rate), so overlaying one axis would flatten the other. */
const SESSION_RATE_METRICS = ['huawei_usg_session_setup_rate'];

/** VPN remote-access user/session gauges (AnyConnect / SSL-VPN logged-in users) on firewalls used
 *  as VPN heads; first match the node collects wins. Cisco RA is a scalar, Fortinet SSL-VPN users a
 *  per-VDOM table (collapsed node-wide via max). */
const VPN_USER_METRICS = ['cisco_ra_sessions', 'fortinet_sslvpn_users'];

/** VPN tunnel-count gauges — site-to-site IPsec/IKE tunnels or GlobalProtect tunnels; first match
 *  wins. All scalar sources here. */
const VPN_TUNNEL_METRICS = [
  'cisco_ipsec_active_tunnels',
  'cisco_ike_active_tunnels',
  'fortinet_vpn_tunnels_up',
  'panos_gp_active_tunnels',
];

/** A session metric resolved against a node's collection set: its name and (for table sources) the
 *  node-level aggregation to apply. */
interface ResolvedSession {
  metric: string;
  /** `max` for table sources (per-entity rows → node value); absent for scalar sources. */
  agg?: 'max';
}

/** Resolve the first candidate session metric the node actually collects, tagging table sources so
 *  the reads aggregate node-wide (`max`) — mirrors the CPU/mem table handling. */
function resolveSession(
  items: { metric_name: string; kind: string }[],
  candidates: string[],
): ResolvedSession | null {
  for (const metric of candidates) {
    const it = items.find((i) => i.metric_name === metric);
    if (it) return { metric, agg: it.kind === 'table' ? 'max' : undefined };
  }
  return null;
}

type MemId = 'huawei' | 'cisco' | 'ucd';

/** Memory sources, in priority order — the first whose two `metrics` are both collected wins.
 *  Each source's pair reduces to used+total bytes (the per-`id` math lives in `deriveMem`,
 *  lib/format), from which the card shows used/total absolute (e.g. "2.6 GB / 3.4 GB") and a
 *  usage-% trend. Table metrics are collapsed node-wide via `max` (consistent with the CPU
 *  gauge); for multi-pool Cisco this approximates the dominant (Processor) pool. */
interface MemSpec {
  id: MemId;
  /** The two raw metrics (both required) that derive used+total; also the chart's inputs. */
  metrics: [string, string];
  /** Scale of the metrics to bytes (1 for byte OIDs, 1024 for KB). */
  unitToBytes: number;
}
const MEM_SPECS: MemSpec[] = [
  { id: 'huawei', metrics: ['huawei_mem_total', 'huawei_mem_free'], unitToBytes: 1 },
  { id: 'cisco', metrics: ['cisco_mem_used', 'cisco_mem_free'], unitToBytes: 1 },
  { id: 'ucd', metrics: ['ucd_mem_total_kb', 'ucd_mem_avail_kb'], unitToBytes: 1024 },
];

/** A memory source resolved against a node's collection set: its two input metrics and unit. */
interface ResolvedMem {
  id: MemId;
  metrics: [string, string];
  unitToBytes: number;
}

/** Device CPU/Memory health: node-level percentages (query-time `max()` across the per-entity
 *  table) with a 0–100% trend chart. Resolves which CPU metric and memory source the node
 *  actually has from its effective collection set, and hides entirely when it has neither. */
function DeviceHealth({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('nodes');
  // `undefined` = still resolving; `null` = resolved, none present.
  const [cpuMetric, setCpuMetric] = useState<string | null | undefined>(undefined);
  const [mem, setMem] = useState<ResolvedMem | null | undefined>(undefined);
  const [sessTotal, setSessTotal] = useState<ResolvedSession | null | undefined>(undefined);
  const [sessRate, setSessRate] = useState<ResolvedSession | null | undefined>(undefined);
  const [vpnUsers, setVpnUsers] = useState<ResolvedSession | null | undefined>(undefined);
  const [vpnTunnels, setVpnTunnels] = useState<ResolvedSession | null | undefined>(undefined);
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      let items: { metric_name: string; kind: string }[] = [];
      try {
        items = await api.listNodeCollection(nodeId, true);
      } catch {
        // admin-only endpoint not permitted → no health card
      }
      const names = new Set(items.map((i) => i.metric_name));
      const cpu = CPU_METRICS.find((m) => names.has(m)) ?? null;
      const spec = MEM_SPECS.find((s) => s.metrics.every((m) => names.has(m)));
      const resolvedMem: ResolvedMem | null = spec
        ? { id: spec.id, metrics: spec.metrics, unitToBytes: spec.unitToBytes }
        : null;
      if (!cancelled) {
        setCpuMetric(cpu);
        setMem(resolvedMem);
        setSessTotal(resolveSession(items, SESSION_TOTAL_METRICS));
        setSessRate(resolveSession(items, SESSION_RATE_METRICS));
        setVpnUsers(resolveSession(items, VPN_USER_METRICS));
        setVpnTunnels(resolveSession(items, VPN_TUNNEL_METRICS));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  // Still resolving any of the cards.
  if (
    cpuMetric === undefined ||
    mem === undefined ||
    sessTotal === undefined ||
    sessRate === undefined ||
    vpnUsers === undefined ||
    vpnTunnels === undefined
  )
    return null;
  if (!cpuMetric && !mem && !sessTotal && !sessRate && !vpnUsers && !vpnTunnels) return null; // nothing to show

  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">{t('overview.deviceHealth')}</div>
        <RangeControl value={range} onChange={setRange} />
      </div>
      <div className="nd-health-metrics">
        {cpuMetric && <CpuHealth nodeId={nodeId} metric={cpuMetric} range={range} />}
        {mem && <MemHealth nodeId={nodeId} mem={mem} range={range} />}
        {sessTotal && (
          <SessionHealth
            nodeId={nodeId}
            label={t('overview.sessions')}
            session={sessTotal}
            range={range}
          />
        )}
        {sessRate && (
          <SessionHealth
            nodeId={nodeId}
            label={t('overview.setupRate')}
            unit="/s"
            session={sessRate}
            range={range}
          />
        )}
        {vpnUsers && (
          <SessionHealth
            nodeId={nodeId}
            label={t('overview.vpnUsers')}
            session={vpnUsers}
            range={range}
          />
        )}
        {vpnTunnels && (
          <SessionHealth
            nodeId={nodeId}
            label={t('overview.vpnTunnels')}
            session={vpnTunnels}
            range={range}
          />
        )}
      </div>
    </section>
  );
}

/** A firewall session gauge (current sessions or setup rate): current value + a trend chart over
 *  the selected window. Reads aggregate node-wide (`max`) for table sources. Unlike CPU/mem the Y
 *  axis is unbounded (counts vary wildly per device), so the chart auto-fits; the axis uses compact
 *  SI suffixes ("12.8k") while the headline + hover show the full count ("12,840"). */
function SessionHealth({
  nodeId,
  label,
  session,
  range,
  unit = '',
}: {
  nodeId: string;
  label: string;
  session: ResolvedSession;
  range: Range;
  unit?: string;
}) {
  const { t } = useTranslation('nodes');
  const [value, setValue] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [win, setWin] = useState<[number, number] | null>(null);
  const tick = useRefreshTick();

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      void Promise.allSettled([
        api.getNodeMetric(nodeId, session.metric, session.agg ? { agg: session.agg } : undefined),
        api.getNodeMetricRange(nodeId, session.metric, {
          from,
          to,
          ...(session.agg ? { agg: session.agg } : {}),
        }),
      ]).then(([v, r]) => {
        if (cancelled) return;
        setValue(v.status === 'fulfilled' ? v.value.value : null);
        setSeries(
          r.status === 'fulfilled' ? pointsToSeries(r.value.points) : { timestamps: [], values: [] },
        );
        setWin([from, to]);
      });
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [nodeId, session.metric, session.agg, range, tick]);

  const fmt = (v: number) => `${formatCount(v)}${unit}`;
  return (
    <div className="nd-health-metric">
      <div className="nd-health-metric-head">
        <span className="nd-health-metric-label">{label}</span>
        <span className="nd-health-metric-value">{value == null ? '—' : fmt(value)}</span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart
          title=""
          timestamps={series.timestamps}
          values={series.values}
          yFormat={formatSi}
          legendFormat={fmt}
          xRange={win ?? undefined}
        />
      ) : (
        <p className="nd-muted">{t('overview.noHistory')}</p>
      )}
    </div>
  );
}

/** CPU health: current % (max aggregate) + a 0–100% trend chart over the selected window. */
function CpuHealth({
  nodeId,
  metric,
  range,
}: {
  nodeId: string;
  metric: string;
  range: Range;
}) {
  const { t } = useTranslation('nodes');
  const [value, setValue] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [win, setWin] = useState<[number, number] | null>(null);
  const tick = useRefreshTick();

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      void Promise.allSettled([
        api.getNodeMetric(nodeId, metric, { agg: 'max' }),
        api.getNodeMetricRange(nodeId, metric, { from, to, agg: 'max' }),
      ]).then(([v, r]) => {
        if (cancelled) return;
        setValue(v.status === 'fulfilled' ? v.value.value : null);
        setSeries(
          r.status === 'fulfilled' ? pointsToSeries(r.value.points) : { timestamps: [], values: [] },
        );
        setWin([from, to]);
      });
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [nodeId, metric, range, tick]);

  return (
    <div className="nd-health-metric">
      <div className="nd-health-metric-head">
        <span className="nd-health-metric-label">{t('overview.cpu')}</span>
        <span className="nd-health-metric-value">{formatUtil(value)}</span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart
          title=""
          timestamps={series.timestamps}
          values={series.values}
          yFormat={formatUtil}
          yRange={PCT_RANGE}
          xRange={win ?? undefined}
        />
      ) : (
        <p className="nd-muted">{t('overview.noHistory')}</p>
      )}
    </div>
  );
}

/** Memory health: current usage as absolute used/total (e.g. "2.6 GB / 3.4 GB") with a 0–100%
 *  usage trend chart. Both the headline and the chart derive from the source's two byte/KB
 *  inputs via `deriveMem`; the chart derives % per point by aligning the inputs on timestamps.
 *  Falls back to a bare % when the total isn't a plausible RAM size. */
function MemHealth({
  nodeId,
  mem,
  range,
}: {
  nodeId: string;
  mem: ResolvedMem;
  range: Range;
}) {
  const { t } = useTranslation('nodes');
  const [usedBytes, setUsedBytes] = useState<number | null>(null);
  const [totalBytes, setTotalBytes] = useState<number | null>(null);
  const [pct, setPct] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [win, setWin] = useState<[number, number] | null>(null);
  const tick = useRefreshTick();

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      void Promise.all([
        Promise.allSettled(mem.metrics.map((m) => api.getNodeMetric(nodeId, m, { agg: 'max' }))),
        Promise.allSettled(
          mem.metrics.map((m) => api.getNodeMetricRange(nodeId, m, { from, to, agg: 'max' })),
        ),
      ]).then(([scalars, ranges]) => {
        if (cancelled) return;
        // Current gauge: name → latest value → derive used/total bytes + %.
        const vals: Record<string, number | null> = {};
        mem.metrics.forEach((m, i) => {
          const s = scalars[i];
          vals[m] = s.status === 'fulfilled' ? s.value.value : null;
        });
        const d = deriveMem(mem.id, vals, mem.unitToBytes);
        setPct(d.pct);
        setUsedBytes(d.usedBytes);
        // Only trust a total that's plausibly a real RAM size (guards bad vendor readings).
        setTotalBytes(d.totalBytes != null && d.totalBytes >= MIN_MEM_TOTAL_BYTES ? d.totalBytes : null);
        // Trend: name → points, then derive % per aligned timestamp.
        const byMetric: Record<string, MetricPoint[]> = {};
        mem.metrics.forEach((m, i) => {
          const r = ranges[i];
          byMetric[m] = r.status === 'fulfilled' ? r.value.points : [];
        });
        setSeries(memPctSeries(mem, byMetric));
        setWin([from, to]);
      });
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [nodeId, mem, range, tick]);

  // Headline: absolute used / total when we have a trustworthy total, else a bare usage %.
  const absolute = usedBytes != null && totalBytes != null;
  return (
    <div className="nd-health-metric">
      <div className="nd-health-metric-head">
        <span className="nd-health-metric-label">{t('overview.memory')}</span>
        <span className="nd-health-metric-value">
          {absolute ? (
            <>
              {formatBytes(usedBytes)}
              <span className="nd-health-metric-sub"> / {formatBytes(totalBytes)}</span>
            </>
          ) : (
            formatUtil(pct)
          )}
        </span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart
          title=""
          timestamps={series.timestamps}
          values={series.values}
          yFormat={formatUtil}
          yRange={PCT_RANGE}
          xRange={win ?? undefined}
        />
      ) : (
        <p className="nd-muted">{t('overview.noHistory')}</p>
      )}
    </div>
  );
}

/** Build the memory usage-% series by aligning a source's two input ranges on shared timestamps
 *  and deriving % per point (`deriveMem`). */
function memPctSeries(
  mem: ResolvedMem,
  byMetric: Record<string, MetricPoint[]>,
): { timestamps: number[]; values: number[] } {
  const [a, b] = mem.metrics;
  const bById = new Map<number, number>();
  for (const p of byMetric[b] ?? []) bById.set(p.t, p.v);
  const timestamps: number[] = [];
  const values: number[] = [];
  for (const p of byMetric[a] ?? []) {
    const vb = bById.get(p.t);
    if (vb == null) continue;
    const { pct } = deriveMem(mem.id, { [a]: p.v, [b]: vb }, mem.unitToBytes);
    if (pct == null) continue;
    timestamps.push(p.t);
    values.push(pct);
  }
  return { timestamps, values };
}

/** Scalars always probed for the System card, on top of any configured ones. */
const BUILTIN_SCALARS = ['snmp_sys_uptime_ticks'];

/** Latest values of the node's scalar SNMP metrics. Hidden when the node has none (e.g. an
 *  ICMP-only node), so it never shows an empty section. */
function SnmpScalars({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('nodes');
  const [readings, setReadings] = useState<{ name: string; value: number }[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const names = new Set(BUILTIN_SCALARS);
      try {
        const items = await api.listNodeCollection(nodeId, true);
        items.filter((i) => i.kind === 'scalar').forEach((i) => names.add(i.metric_name));
      } catch {
        // fall back to the built-in scalars
      }
      // Fetch every scalar concurrently (was a per-metric await waterfall — a dozen serial
      // round-trips on open and on each poll). Order is preserved from `nameList`; a metric with
      // no reading yet is simply dropped. Matches the sibling loaders (UrlHealth/CpuHealth/…).
      const nameList = [...names];
      const results = await Promise.allSettled(
        nameList.map((name) => api.getNodeMetric(nodeId, name)),
      );
      const out: { name: string; value: number }[] = [];
      results.forEach((res, i) => {
        if (res.status === 'fulfilled') {
          out.push({ name: nameList[i], value: res.value.value });
        }
      });
      if (!cancelled) setReadings(out);
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  if (!readings || readings.length === 0) return null;
  return (
    <section>
      <div className="nd-section-t">{t('overview.systemSnmp')}</div>
      <div className="nd-scalars">
        {readings.map((r) => {
          const d = scalarDisplay(r.name, r.value);
          return (
            <div className="nd-scalar" key={r.name}>
              <span className={`nd-scalar-name${d.known ? '' : ' mono'}`}>{d.label}</span>
              <span className={`nd-scalar-value${d.known ? '' : ' mono'}`}>{d.value}</span>
            </div>
          );
        })}
      </div>
    </section>
  );
}
