// Overview tab of the unified node detail. The compact core (per the redesign) is an "ICMP RTT ·
// last 30 min" sparkline + a two-column facts grid. Below it sit the richer, relocated sections —
// Active alerts, Device health (CPU/Mem), System (SNMP) scalars — each of which self-hides when the
// node has no such data, so a simple ICMP-only node shows just sparkline + facts, while a fully
// monitored device shows everything. Nothing from the old detail page is dropped, only restyled.

import { useEffect, useState } from 'react';
import { api } from '../../services/api';
import {
  formatTimestamp,
  formatUptimeTicks,
  formatUtil,
  pointsToSeries,
  scalarDisplay,
  severityColorVar,
  stateLabel,
} from '../../lib/format';
import { groupPath } from '../../lib/nodeTree';
import type {
  NodeDetail,
  NodeGroup,
  NodeState,
  NodeStatus,
  NodeSummary,
} from '../../types/api';
import { MetricChart } from '../MetricChart/MetricChart';
import { Button } from '../ui/Button';

const STATUS_REFRESH_MS = 15_000;

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
  const facts = useFacts(node, groups, nodes, unreachable);
  return (
    <div className="nd-overview">
      <section>
        <div className="nd-section-t">ICMP RTT · last 30 min</div>
        {series.timestamps.length > 0 ? (
          <MetricChart
            title=""
            timestamps={series.timestamps}
            values={series.values}
            height={96}
            yFormat={(v) => `${Math.round(v)}`}
          />
        ) : (
          <p className="nd-muted nd-spark-empty">
            {unreachable ? '— (unreachable)' : 'No RTT history yet…'}
          </p>
        )}
      </section>

      <div className="nd-facts">
        {facts.map((f) => (
          <div key={f.label}>
            <div className="nd-fact-k">{f.label}</div>
            <div className={`nd-fact-v${f.mono ? ' mono' : ''}`}>{f.value}</div>
          </div>
        ))}
      </div>

      {status && status.alerts.length > 0 && (
        <section>
          <div className="nd-section-t">Active alerts</div>
          <div className="nd-alerts">
            {status.alerts.map((a) => (
              <div className="nd-alert" key={`${a.check}|${a.severity}`}>
                <span
                  className="nd-alert-dot"
                  style={{ background: severityColorVar(a.severity) }}
                />
                <span className="nd-alert-state">{stateLabel(a.state)}</span>
                {a.root_cause && (
                  <span className="nd-muted mono nd-alert-cause">← caused by {a.root_cause}</span>
                )}
                {a.flapping && <span className="nd-alert-flap">flapping</span>}
                <span className="nd-muted nd-alert-time">{formatTimestamp(a.at_unix_ms)}</span>
              </div>
            ))}
          </div>
        </section>
      )}

      <DeviceHealth nodeId={node.id} />
      <SnmpScalars nodeId={node.id} />
    </div>
  );
}

interface Fact {
  label: string;
  value: string;
  mono?: boolean;
}

/** Resolve the facts-grid rows for a node: group breadcrumb, address, maker/model, the
 *  profile/credential names (looked up by id), the parent-node name, and uptime. */
function useFacts(
  node: NodeDetail,
  groups: NodeGroup[],
  nodes: NodeSummary[] | undefined,
  unreachable: boolean,
): Fact[] {
  const [profileName, setProfileName] = useState<string | null>(null);
  const [credentialName, setCredentialName] = useState<string | null>(null);
  const [parentName, setParentName] = useState<string | null>(null);
  const [uptime, setUptime] = useState<string | null>(null);

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

  const path = groupPath(groups, node.group_id);
  return [
    { label: 'Group', value: path.length ? path.join(' / ') : 'Ungrouped' },
    { label: 'IP address', value: node.address, mono: true },
    { label: 'Maker', value: node.vendor || '—' },
    { label: 'Model', value: node.model || '—', mono: true },
    { label: 'Device profile', value: profileName ?? '—' },
    { label: 'SNMP credential', value: credentialName ?? '—' },
    { label: 'Parent node', value: parentName ?? '—', mono: !!parentName },
    {
      label: 'Uptime',
      value: unreachable ? '— (unreachable)' : (uptime ?? '—'),
    },
  ];
}

/** ifOperStatus (1 = up) → a node-state colour for status dots/charts. (Shared with the
 *  Interfaces tab via a re-export.) */
export function operState(oper: number | null): NodeState {
  if (oper == null) return 'unknown';
  return oper === 1 ? 'ok' : 'critical';
}

/** Chart time-windows for the device-health and interface trend charts. */
export const RANGES: { label: string; secs: number }[] = [
  { label: '1h', secs: 3600 },
  { label: '6h', secs: 6 * 3600 },
  { label: '24h', secs: 24 * 3600 },
];

/** Vendor/host health gauges that read as 0–100%, by role (cisco_mem_used/free are bytes and
 *  HOST-RESOURCES memory needs a used/size ratio — both out of scope here). */
const HEALTH_METRICS: { metric: string; role: 'cpu' | 'mem' }[] = [
  { metric: 'huawei_cpu_usage', role: 'cpu' },
  { metric: 'cisco_cpu_5min', role: 'cpu' },
  { metric: 'hr_processor_load', role: 'cpu' },
  { metric: 'huawei_mem_usage', role: 'mem' },
];
const HEALTH_ROLE_LABEL: Record<'cpu' | 'mem', string> = { cpu: 'CPU', mem: 'Memory' };
const HEALTH_ROLES = ['cpu', 'mem'] as const;

/** Device CPU/Memory health: node-level percentages (query-time `max()` across the per-entity
 *  table) with a trend chart. Resolves which health metrics the node actually has from its
 *  effective collection set and hides entirely when it has none. */
function DeviceHealth({ nodeId }: { nodeId: string }) {
  const [picked, setPicked] = useState<{ role: 'cpu' | 'mem'; metric: string }[] | null>(null);
  const [rangeSecs, setRangeSecs] = useState(RANGES[0].secs);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      let names = new Set<string>();
      try {
        const items = await api.listNodeCollection(nodeId, true);
        names = new Set(items.map((i) => i.metric_name));
      } catch {
        // admin-only endpoint not permitted → no health card
      }
      const out = HEALTH_ROLES.flatMap((role) => {
        const hit = HEALTH_METRICS.find((h) => h.role === role && names.has(h.metric));
        return hit ? [{ role, metric: hit.metric }] : [];
      });
      if (!cancelled) setPicked(out);
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  if (!picked || picked.length === 0) return null;
  return (
    <section>
      <div className="nd-section-head">
        <div className="nd-section-t">Device health</div>
        <div className="nd-windows">
          {RANGES.map((r) => (
            <Button
              key={r.secs}
              variant={rangeSecs === r.secs ? 'primary' : 'outline'}
              onClick={() => setRangeSecs(r.secs)}
            >
              {r.label}
            </Button>
          ))}
        </div>
      </div>
      <div className="nd-health-metrics">
        {picked.map((p) => (
          <HealthMetric
            key={p.metric}
            nodeId={nodeId}
            metric={p.metric}
            label={HEALTH_ROLE_LABEL[p.role]}
            rangeSecs={rangeSecs}
          />
        ))}
      </div>
    </section>
  );
}

/** One health metric: current % (max aggregate) + a trend chart over the selected window. */
function HealthMetric({
  nodeId,
  metric,
  label,
  rangeSecs,
}: {
  nodeId: string;
  metric: string;
  label: string;
  rangeSecs: number;
}) {
  const [value, setValue] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const to = Math.floor(Date.now() / 1000);
      void Promise.allSettled([
        api.getNodeMetric(nodeId, metric, { agg: 'max' }),
        api.getNodeMetricRange(nodeId, metric, { from: to - rangeSecs, to, agg: 'max' }),
      ]).then(([v, r]) => {
        if (cancelled) return;
        setValue(v.status === 'fulfilled' ? v.value.value : null);
        setSeries(
          r.status === 'fulfilled' ? pointsToSeries(r.value.points) : { timestamps: [], values: [] },
        );
      });
    };
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId, metric, rangeSecs]);

  return (
    <div className="nd-health-metric">
      <div className="nd-health-metric-head">
        <span className="nd-health-metric-label">{label}</span>
        <span className="nd-health-metric-value">{formatUtil(value)}</span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart title="" timestamps={series.timestamps} values={series.values} yFormat={formatUtil} />
      ) : (
        <p className="nd-muted">No history yet…</p>
      )}
    </div>
  );
}

/** Scalars always probed for the System card, on top of any configured ones. */
const BUILTIN_SCALARS = ['snmp_sys_uptime_ticks'];

/** Latest values of the node's scalar SNMP metrics. Hidden when the node has none (e.g. an
 *  ICMP-only node), so it never shows an empty section. */
function SnmpScalars({ nodeId }: { nodeId: string }) {
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
      const out: { name: string; value: number }[] = [];
      for (const name of names) {
        try {
          const r = await api.getNodeMetric(nodeId, name);
          out.push({ name, value: r.value });
        } catch {
          // no reading for this metric yet
        }
      }
      if (!cancelled) setReadings(out);
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  if (!readings || readings.length === 0) return null;
  return (
    <section>
      <div className="nd-section-t">System (SNMP)</div>
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
