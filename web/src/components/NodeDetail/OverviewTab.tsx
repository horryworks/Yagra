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
  NodeStatus,
  NodeSummary,
} from '../../types/api';
import { MetricChart } from '../MetricChart/MetricChart';
import {
  hasAnyHealth,
  METRIC_CARDS,
  resolveHealth,
  type MetricCardSpec,
  type ResolvedHealth,
  type ResolvedMem,
  type ResolvedMetric,
} from './metricCards';
import { formatKb, memPctSeries } from './overviewMetrics';
import { overviewShowsIcmp, visibleFactRows, type FactRow } from './overviewFacts';
import { overviewScalars, viewOf } from '../../lib/metricInventory';
import { fetchNodeMetrics } from '../../lib/metricInventoryCache';
import { certTone, httpToneVar } from './healthTone';
import { extractMetricKey, formatExtractedValue, metricsFromKey } from './urlExtracts';
import { RangeControl, resolveRange, type Range } from './RangeControl';
import { DnsHealth } from './DnsHealth';
import { CheckConfigActions } from './CheckConfigActions';
import { useRangeStore } from '../../store';
import { useRefreshTick } from '../../lib/refreshTick';

interface Props {
  node: NodeDetail;
  groups: NodeGroup[];
  nodes?: NodeSummary[];
  status: NodeStatus | null;
  /** The kind's liveness history (last ~30 min), shared with the header's "seen" line. Charted
   *  here only for an ordinary device, where it is `icmp_rtt_ms`; for the monitor kinds it is
   *  their own up-gauge and exists for the timestamp — see `NODE_KIND_SPEC.livenessMetric`. */
  series: { timestamps: number[]; values: number[] };
  unreachable: boolean;
  /** Whether the viewer may reconfigure monitoring (ManageConfig). Gates the check-config edit. */
  canEdit?: boolean;
  /** Re-fetch the node after its check config changed, so the card shows what was just saved. */
  onChanged?: () => void;
}

export function OverviewTab({
  node,
  groups,
  nodes,
  status,
  series,
  unreachable,
  canEdit = false,
  onChanged,
}: Props) {
  const { t } = useTranslation('nodes');
  const facts = useFacts(node, groups, nodes, unreachable);
  return (
    <div className="nd-overview">
      {/* Only an ordinary device is pinged at all — see `overviewShowsIcmp`. For the monitor kinds
          the equivalent chart lives in their own health card below, over their own metric. */}
      {overviewShowsIcmp(node.kind) && (
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
      )}

      <div className="nd-facts">
        {visibleFactRows(node.kind).map((row) => {
          const f = facts[row];
          return (
            <div key={row}>
              <div className="nd-fact-k">{f.label}</div>
              <div className={`nd-fact-v${f.mono ? ' mono' : ''}${f.warn ? ' warn' : ''}`}>
                {f.value}
              </div>
            </div>
          );
        })}
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
        <UrlHealth
          nodeId={node.id}
          url={node.url_check.url}
          // The monitor's own rules are the only thing that knows which operator-named metrics
          // exist — they are not in any allowlist, by design (ADR-047 Inc.3).
          extracts={node.url_check.json_extract ?? []}
          actions={
            canEdit ? <CheckConfigActions nodeId={node.id} kind="url" onChanged={onChanged} /> : null
          }
        />
      )}
      {node.kind === 'dns' && node.dns_check && (
        <DnsHealth
          nodeId={node.id}
          check={node.dns_check}
          actions={
            canEdit ? <CheckConfigActions nodeId={node.id} kind="dns" onChanged={onChanged} /> : null
          }
        />
      )}
      {node.kind === 'meraki' && node.meraki_device && (
        <MerakiHealth nodeId={node.id} device={node.meraki_device} />
      )}
      <DeviceHealth nodeId={node.id} />
      <SnmpScalars nodeId={node.id} />
    </div>
  );
}

/** URL-monitor health: availability (`http_up`), the last HTTP status code, the response time, and
 *  (for HTTPS) the TLS certificate's days-to-expiry. Mirrors the Device-health card layout;
 *  self-refreshes. Shown only for URL-monitor nodes (the caller guards on `node.url_check`).
 *
 *  The response time is time-to-response-headers, not time-to-body-complete — the probe never reads
 *  the body. The label says so, because "response time" otherwise reads as the whole transfer. */
function UrlHealth({
  nodeId,
  url,
  extracts,
  actions,
}: {
  nodeId: string;
  url: string;
  extracts: { metric: string; path: string }[];
  actions?: React.ReactNode;
}) {
  const { t } = useTranslation('nodes');
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);
  const tick = useRefreshTick();
  const [up, setUp] = useState<number | null>(null);
  const [statusCode, setStatusCode] = useState<number | null>(null);
  const [certDays, setCertDays] = useState<number | null>(null);
  const [responseMs, setResponseMs] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [responseSeries, setResponseSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [bodyMatch, setBodyMatch] = useState<number | null>(null);
  const [bodyTruncated, setBodyTruncated] = useState<number | null>(null);
  // Keyed by metric name — a rule whose path found nothing records no sample at all, so a missing
  // entry here is the honest "no reading", not a zero (ADR-047 決定 3).
  const [extracted, setExtracted] = useState<Record<string, number>>({});
  const [win, setWin] = useState<[number, number] | null>(null);

  // The rule list drives the fetch, so it must be a stable dependency rather than a new array
  // identity on every render — otherwise the effect re-runs forever. Key and parse are one pair in
  // `urlExtracts.ts`; they used to be two independent spellings here and disagreed.
  const extractNames = extractMetricKey(extracts);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      void Promise.allSettled([
        api.getNodeMetric(nodeId, 'http_up'),
        api.getNodeMetric(nodeId, 'http_status_code'),
        api.getNodeMetric(nodeId, 'ssl_cert_days_to_expiry'),
        api.getNodeMetricRange(nodeId, 'http_status_code', { from, to }),
        api.getNodeMetric(nodeId, 'http_response_time_ms'),
        api.getNodeMetricRange(nodeId, 'http_response_time_ms', { from, to }),
        api.getNodeMetric(nodeId, 'http_body_match'),
        api.getNodeMetric(nodeId, 'http_body_truncated'),
      ]).then(([u, s, c, r, rt, rtr, bm, bt]) => {
        if (cancelled) return;
        setUp(u.status === 'fulfilled' ? u.value.value : null);
        setStatusCode(s.status === 'fulfilled' ? s.value.value : null);
        setCertDays(c.status === 'fulfilled' ? c.value.value : null);
        setSeries(
          r.status === 'fulfilled' ? pointsToSeries(r.value.points) : { timestamps: [], values: [] },
        );
        setResponseMs(rt.status === 'fulfilled' ? rt.value.value : null);
        setResponseSeries(
          rtr.status === 'fulfilled'
            ? pointsToSeries(rtr.value.points)
            : { timestamps: [], values: [] },
        );
        setBodyMatch(bm.status === 'fulfilled' ? bm.value.value : null);
        setBodyTruncated(bt.status === 'fulfilled' ? bt.value.value : null);
        setWin([from, to]);
      });
      const names = metricsFromKey(extractNames);
      if (names.length > 0) {
        void Promise.allSettled(names.map((m) => api.getNodeMetric(nodeId, m))).then((rs) => {
          if (cancelled) return;
          const next: Record<string, number> = {};
          rs.forEach((r, i) => {
            // A rule that has never produced a sample simply has no key — the card is then absent
            // rather than showing a 0 the monitor never observed.
            if (r.status === 'fulfilled' && r.value.value != null) next[names[i]] = r.value.value;
          });
          setExtracted(next);
        });
      } else {
        setExtracted({});
      }
    };
    load();
    return () => {
      cancelled = true;
    };
  }, [nodeId, range, tick, extractNames]);

  const code = statusCode == null ? null : Math.round(statusCode);
  const availabilityTone: 'up' | 'critical' = up === 1 ? 'up' : 'critical';

  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">{t('overview.urlMonitor')}</div>
        <RangeControl value={range} onChange={setRange} />
        {actions}
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
        {/* Absent while the endpoint is unreachable — the poller writes no response time when
            there was no response, so the card disappears rather than showing the timeout. */}
        {responseMs != null && (
          <div className="nd-health-metric">
            <div className="nd-health-metric-head">
              <span className="nd-health-metric-label">{t('overview.responseTime')}</span>
              <span className="nd-health-metric-value">{Math.round(responseMs)} ms</span>
            </div>
            {responseSeries.timestamps.length > 0 && (
              <MetricChart
                title=""
                timestamps={responseSeries.timestamps}
                values={responseSeries.values}
                yFormat={(v) => `${Math.round(v)} ms`}
                legendFormat={(v) => `${Math.round(v)} ms`}
                xRange={win ?? undefined}
              />
            )}
          </div>
        )}
        {/* Only for a monitor that carries a content rule — the poller emits nothing otherwise.
            A 0 has two causes and the operator needs to know which: the keyword really was
            wrong, or the page outgrew its read budget so the rule could not be decided. The
            truncation line is the only thing that tells them apart. */}
        {bodyMatch != null && (
          <div className="nd-health-metric">
            <div className="nd-health-metric-head">
              <span className="nd-health-metric-label">{t('overview.bodyMatch')}</span>
              <span
                className="nd-health-metric-value"
                style={{ color: httpToneVar(bodyMatch === 1 ? 'up' : 'critical') }}
              >
                {bodyMatch === 1 ? t('overview.bodyMatchOk') : t('overview.bodyMatchFailed')}
              </span>
            </div>
            {bodyTruncated === 1 && (
              <div className="nd-url-status-line">
                <span className="nd-muted">{t('overview.bodyTruncated')}</span>
              </div>
            )}
          </div>
        )}
        {/* Operator-named values lifted out of the JSON body. The label is the metric name the
            operator chose — there is nothing to translate, and inventing a prettier one would
            hide the string they have to type into a threshold. A rule with no reading yet is
            simply absent (the poller records nothing rather than a zero). */}
        {extracts.map((e) =>
          extracted[e.metric] == null ? null : (
            <div className="nd-health-metric" key={e.metric}>
              <div className="nd-health-metric-head">
                <span className="nd-health-metric-label mono">{e.metric}</span>
                <span className="nd-health-metric-value">
                  {formatExtractedValue(extracted[e.metric])}
                </span>
              </div>
              <div className="nd-url-status-line">
                <span className="nd-muted mono">{e.path}</span>
              </div>
            </div>
          ),
        )}
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

/** Resolve every facts-grid row for a node: group breadcrumb, poll-pool + current poller, address,
 *  maker/model, the profile/credential names (looked up by id), the parent-node name, uptime and
 *  the DNS resolver.
 *
 *  Returns all of them keyed by [`FactRow`] — the caller renders `visibleFactRows(kind)`. Building
 *  the full record and filtering at render (rather than assembling a per-kind array here) is what
 *  makes the map exhaustive to the compiler: a new row cannot be added without a value. */
function useFacts(
  node: NodeDetail,
  groups: NodeGroup[],
  nodes: NodeSummary[] | undefined,
  unreachable: boolean,
): Record<FactRow, Fact> {
  const { t } = useTranslation('nodes');
  const [profileName, setProfileName] = useState<string | null>(null);
  const [credentialName, setCredentialName] = useState<string | null>(null);
  const [parentName, setParentName] = useState<string | null>(null);
  const [uptime, setUptime] = useState<string | null>(null);
  const [assignment, setAssignment] = useState<NodeAssignment | undefined>(undefined);

  // The rows a kind does not show are not fetched for. `visibleFactRows` is the one place that
  // decides, so a row and the request that fills it cannot disagree about whether it is needed.
  const rows = visibleFactRows(node.kind);
  const wantsCredential = rows.includes('credential');
  const wantsUptime = rows.includes('uptime');

  useEffect(() => {
    let cancelled = false;
    if (node.profile_id) {
      api
        .listProfiles()
        .then((ps) => !cancelled && setProfileName(ps.find((p) => p.id === node.profile_id)?.name ?? null))
        .catch(() => undefined);
    } else setProfileName(null);
    if (node.credential_id && wantsCredential) {
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
  }, [node.profile_id, node.credential_id, wantsCredential]);

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

  // Uptime from the SNMP sysUpTime scalar (TimeTicks). Only asked for when the row is shown — the
  // monitor kinds are never SNMP-walked, so this could only ever 404 for them.
  useEffect(() => {
    let cancelled = false;
    setUptime(null);
    if (!wantsUptime) return;
    api
      .getNodeMetric(node.id, 'snmp_sys_uptime_ticks')
      .then((r) => !cancelled && setUptime(formatUptimeTicks(r.value)))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [node.id, wantsUptime]);

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

  const path = groupPath(groups, node.group_id ?? null);
  const groupName = (id: string) => groups.find((g) => g.id === id)?.name;
  return {
    group: { label: t('field.group'), value: path.length ? path.join(' / ') : t('ungrouped') },
    // Placement facts sit together: which folder, which pool, which poller.
    pool: { label: t('field.pool'), value: poolFactLabel(assignment, groupName, t), mono: true },
    polledBy: {
      label: t('field.polledBy'),
      value: polledByLabel(assignment?.polled_by, t),
      mono: assignment?.polled_by.state === 'assigned',
      warn: polledByIsWarning(assignment?.polled_by),
    },
    address: { label: t('field.ipAddress'), value: node.address, mono: true },
    maker: { label: t('field.maker'), value: node.vendor || '—' },
    model: { label: t('field.model'), value: node.model || '—', mono: true },
    profile: { label: t('field.deviceProfile'), value: profileName ?? '—' },
    credential: { label: t('field.snmpCredential'), value: credentialName ?? '—' },
    parent: { label: t('field.parentNode'), value: parentName ?? '—', mono: !!parentName },
    uptime: {
      label: t('field.uptime'),
      value: unreachable ? t('overview.unreachableDash') : (uptime ?? '—'),
    },
    // DNS monitors only: the resolver being queried. Blank means the poller container's own
    // resolver, which is a real answer, not a missing one.
    resolver: {
      label: t('field.resolver'),
      value: node.dns_check?.resolver ?? t('overview.systemResolver'),
      mono: !!node.dns_check?.resolver,
    },
  };
}

/** ifOperStatus (1 = up) → a node-state colour for status dots/charts. (Shared with the
 *  Interfaces tab via a re-export.) */

/** A bounded gauge's Y range — CPU/Mem read as 0–100%, so the chart baseline is 0, not the
 *  window's min. Module-level + stable so MetricChart isn't rebuilt on every refresh. */
const PCT_RANGE: [number, number] = [0, 100];

/** A real device's total RAM is never a handful of bytes — below this floor a "total memory"
 *  reading is bogus (e.g. a vendor size OID unsupported on the device, or one whose value
 *  overflows a 32-bit SNMP INTEGER so the node-wide `max()` lands on a tiny wrong row). When
 *  the total isn't trustworthy we show the % only rather than a wrong size like "90 B". */
const MIN_MEM_TOTAL_BYTES = 1024 * 1024;

/** Device health: every gauge in [`METRIC_CARDS`] the node actually collects, plus its memory
 *  source, over the shared range control. Resolution happens once, in `resolveHealth`, so the two
 *  render guards below are derived from the card list rather than repeating it — forgetting a card
 *  in either guard used to hide the *whole* section, silently, for every node. */
function DeviceHealth({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('nodes');
  // `null` = still resolving. Once set, every card's presence is known at once.
  const [health, setHealth] = useState<ResolvedHealth | null>(null);
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      // The metric inventory, not the collection set: it needs only read permission (the collection
      // endpoint is ManageConfig, so this whole section used to be invisible to a viewer), and it
      // reports whether each metric has actually reported rather than only that it was asked for.
      // Through the shared cache, so this and the scalar strip below make one request between them
      // (and so the dashboard's fleet Top-N learns the names this node has).
      const items = await fetchNodeMetrics(nodeId);
      if (!cancelled) setHealth(resolveHealth(items));
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  if (!health) return null; // still resolving
  if (!hasAnyHealth(health)) return null; // nothing to show

  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">{t('overview.deviceHealth')}</div>
        <RangeControl value={range} onChange={setRange} />
      </div>
      <div className="nd-health-metrics">
        {METRIC_CARDS.map((spec) => {
          const resolved = health.cards[spec.id];
          return (
            resolved && (
              <MetricCard key={spec.id} nodeId={nodeId} spec={spec} resolved={resolved} range={range} />
            )
          );
        })}
        {health.mem && <MemHealth nodeId={nodeId} mem={health.mem} range={range} />}
      </div>
    </section>
  );
}

/** One Device-health gauge: current value + a trend chart over the selected window, reading
 *  node-wide (`max`) for table sources.
 *
 *  The two scales were two components that differed only in how a number is rendered. A `percent`
 *  card pins the Y axis to 0–100 so a CPU hovering at 40% doesn't fill the chart; a `count` card
 *  auto-fits, since session counts vary by orders of magnitude per device, and its axis uses compact
 *  SI suffixes ("12.8k") while the headline and hover show the full count ("12,840"). */
function MetricCard({
  nodeId,
  spec,
  resolved,
  range,
}: {
  nodeId: string;
  spec: MetricCardSpec;
  resolved: ResolvedMetric;
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
  const { metric, agg } = resolved;

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      void Promise.allSettled([
        api.getNodeMetric(nodeId, metric, agg ? { agg } : undefined),
        api.getNodeMetricRange(nodeId, metric, { from, to, ...(agg ? { agg } : {}) }),
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
  }, [nodeId, metric, agg, range, tick]);

  const pct = spec.scale === 'percent';
  const fmt = (v: number) => (pct ? formatUtil(v) : `${formatCount(v)}${spec.unit ?? ''}`);
  return (
    <div className="nd-health-metric">
      <div className="nd-health-metric-head">
        <span className="nd-health-metric-label">{t(spec.labelKey)}</span>
        <span className="nd-health-metric-value">{value == null ? '—' : fmt(value)}</span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart
          title=""
          timestamps={series.timestamps}
          values={series.values}
          yFormat={pct ? formatUtil : formatSi}
          yRange={pct ? PCT_RANGE : undefined}
          legendFormat={pct ? undefined : fmt}
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

/** Latest values of the node's node-level metrics. Hidden when the node has none (e.g. an
 *  ICMP-only node), so it never shows an empty section.
 *
 *  Sourced from the metric inventory, which is what lets this show metrics no collection set
 *  contains — the neighbour count, the URL/DNS monitor gauges, values extracted from a monitored
 *  JSON response — and lets a viewer see it at all. `overviewScalars` decides what belongs here:
 *  counters have no glanceable value, and per-interface metrics have their own tab. */
function SnmpScalars({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('nodes');
  const [readings, setReadings] = useState<{ name: string; value: number }[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      // An empty inventory (skeleton mode, or the node is gone) means nothing to probe; the section
      // hides itself below.
      const shown = overviewScalars(await fetchNodeMetrics(nodeId));
      // Fetch every reading concurrently (was a per-metric await waterfall — a dozen serial
      // round-trips on open and on each poll). Order is preserved; a metric with no reading yet is
      // simply dropped. Matches the sibling loaders (UrlHealth/CpuHealth/…).
      const results = await Promise.allSettled(
        shown.map((e) =>
          api.getNodeMetric(
            nodeId,
            e.metric,
            viewOf(e).read.kind === 'aggregate' ? { agg: 'max' } : undefined,
          ),
        ),
      );
      const out: { name: string; value: number }[] = [];
      results.forEach((res, i) => {
        if (res.status === 'fulfilled') {
          out.push({ name: shown[i].metric, value: res.value.value });
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
