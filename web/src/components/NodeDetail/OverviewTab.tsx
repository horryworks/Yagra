// Overview tab of the unified node detail. The compact core (per the redesign) is an "ICMP RTT ·
// last 30 min" sparkline + a two-column facts grid. Below it sit the richer, relocated sections —
// Active alerts, Device health (CPU/Mem), System (SNMP) scalars — each of which self-hides when the
// node has no such data, so a simple ICMP-only node shows just sparkline + facts, while a fully
// monitored device shows everything. Nothing from the old detail page is dropped, only restyled.

import { useEffect, useMemo, useState } from 'react';
import { api } from '../../services/api';
import {
  deriveMem,
  formatBytes,
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
  MetricPoint,
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

type MemId = 'huawei' | 'cisco' | 'ucd';

/** Memory sources, in priority order — the first whose `require` metrics are all collected wins.
 *  Each yields a current utilization % and, where the size is collected, a total byte count shown
 *  alongside it (e.g. "62% / 32 GB"). The per-`id` math lives in `deriveMem` (lib/format). Table
 *  metrics are collapsed node-wide via `max` (consistent with the CPU gauge); for multi-pool Cisco
 *  this approximates the dominant (Processor) pool. */
interface MemSpec {
  id: MemId;
  /** Metrics that must all be present for this source to apply (also the % inputs). */
  require: string[];
  /** Metrics + unit that yield the total size; absent on the node ⇒ % only, no total. */
  total: { metrics: string[]; unitToBytes: number };
}
const MEM_SPECS: MemSpec[] = [
  {
    id: 'huawei',
    require: ['huawei_mem_usage'],
    total: { metrics: ['huawei_mem_size'], unitToBytes: 1 },
  },
  {
    id: 'cisco',
    require: ['cisco_mem_used', 'cisco_mem_free'],
    total: { metrics: ['cisco_mem_used', 'cisco_mem_free'], unitToBytes: 1 },
  },
  {
    id: 'ucd',
    require: ['ucd_mem_total_kb', 'ucd_mem_avail_kb'],
    total: { metrics: ['ucd_mem_total_kb'], unitToBytes: 1024 },
  },
];

/** A memory source resolved against a node's collection set: the % inputs, whichever total
 *  metrics it actually has (empty ⇒ no total), and the unit scale. */
interface ResolvedMem {
  id: MemId;
  metrics: string[];
  totalMetrics: string[];
  totalUnitToBytes: number;
}

/** Device CPU/Memory health: node-level percentages (query-time `max()` across the per-entity
 *  table) with a 0–100% trend chart. Resolves which CPU metric and memory source the node
 *  actually has from its effective collection set, and hides entirely when it has neither. */
function DeviceHealth({ nodeId }: { nodeId: string }) {
  // `undefined` = still resolving; `null` = resolved, none present.
  const [cpuMetric, setCpuMetric] = useState<string | null | undefined>(undefined);
  const [mem, setMem] = useState<ResolvedMem | null | undefined>(undefined);
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
      const cpu = CPU_METRICS.find((m) => names.has(m)) ?? null;
      const spec = MEM_SPECS.find((s) => s.require.every((m) => names.has(m)));
      const resolvedMem: ResolvedMem | null = spec
        ? {
            id: spec.id,
            metrics: spec.require,
            totalMetrics: spec.total.metrics.every((m) => names.has(m)) ? spec.total.metrics : [],
            totalUnitToBytes: spec.total.unitToBytes,
          }
        : null;
      if (!cancelled) {
        setCpuMetric(cpu);
        setMem(resolvedMem);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

  if (cpuMetric === undefined || mem === undefined) return null; // still resolving
  if (!cpuMetric && !mem) return null; // nothing to show

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
        {cpuMetric && <CpuHealth nodeId={nodeId} metric={cpuMetric} rangeSecs={rangeSecs} />}
        {mem && <MemHealth nodeId={nodeId} mem={mem} rangeSecs={rangeSecs} />}
      </div>
    </section>
  );
}

/** CPU health: current % (max aggregate) + a 0–100% trend chart over the selected window. */
function CpuHealth({
  nodeId,
  metric,
  rangeSecs,
}: {
  nodeId: string;
  metric: string;
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
        <span className="nd-health-metric-label">CPU</span>
        <span className="nd-health-metric-value">{formatUtil(value)}</span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart
          title=""
          timestamps={series.timestamps}
          values={series.values}
          yFormat={formatUtil}
          yRange={PCT_RANGE}
        />
      ) : (
        <p className="nd-muted">No history yet…</p>
      )}
    </div>
  );
}

/** Memory health: current utilization % plus the total size (e.g. "62% / 32 GB") and a 0–100%
 *  trend chart. The %/total derive per source shape (`deriveMem`); the chart derives % per point
 *  by aligning the source's input series on their timestamps. */
function MemHealth({
  nodeId,
  mem,
  rangeSecs,
}: {
  nodeId: string;
  mem: ResolvedMem;
  rangeSecs: number;
}) {
  const [pct, setPct] = useState<number | null>(null);
  const [totalBytes, setTotalBytes] = useState<number | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });

  // Scalars to fetch for the current gauge: the % inputs plus any total metrics present.
  const gaugeMetrics = useMemo(
    () => Array.from(new Set([...mem.metrics, ...mem.totalMetrics])),
    [mem],
  );

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const to = Math.floor(Date.now() / 1000);
      void Promise.all([
        Promise.allSettled(gaugeMetrics.map((m) => api.getNodeMetric(nodeId, m, { agg: 'max' }))),
        Promise.allSettled(
          mem.metrics.map((m) =>
            api.getNodeMetricRange(nodeId, m, { from: to - rangeSecs, to, agg: 'max' }),
          ),
        ),
      ]).then(([scalars, ranges]) => {
        if (cancelled) return;
        // Current gauge: name → latest value → derive % + total.
        const vals: Record<string, number | null> = {};
        gaugeMetrics.forEach((m, i) => {
          const s = scalars[i];
          vals[m] = s.status === 'fulfilled' ? s.value.value : null;
        });
        const d = deriveMem(mem.id, vals, mem.totalUnitToBytes);
        setPct(d.pct);
        // Only surface a total we actually have and that's plausibly a real RAM size.
        const total = d.totalBytes;
        setTotalBytes(
          mem.totalMetrics.length > 0 && total != null && total >= MIN_MEM_TOTAL_BYTES
            ? total
            : null,
        );
        // Trend: name → points, then derive % per aligned timestamp.
        const byMetric: Record<string, MetricPoint[]> = {};
        mem.metrics.forEach((m, i) => {
          const r = ranges[i];
          byMetric[m] = r.status === 'fulfilled' ? r.value.points : [];
        });
        setSeries(memPctSeries(mem, byMetric));
      });
    };
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId, mem, rangeSecs, gaugeMetrics]);

  return (
    <div className="nd-health-metric">
      <div className="nd-health-metric-head">
        <span className="nd-health-metric-label">Memory</span>
        <span className="nd-health-metric-value">
          {formatUtil(pct)}
          {totalBytes != null && (
            <span className="nd-health-metric-sub"> / {formatBytes(totalBytes)}</span>
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
        />
      ) : (
        <p className="nd-muted">No history yet…</p>
      )}
    </div>
  );
}

/** Build the memory utilization % series from a source's input ranges. A single-input source
 *  (Huawei) is already a %, so it passes through; a two-input source (Cisco used/free, UCD
 *  total/avail) aligns its inputs on shared timestamps and derives % per point. */
function memPctSeries(
  mem: ResolvedMem,
  byMetric: Record<string, MetricPoint[]>,
): { timestamps: number[]; values: number[] } {
  if (mem.metrics.length === 1) {
    return pointsToSeries(byMetric[mem.metrics[0]] ?? []);
  }
  const [a, b] = mem.metrics;
  const bById = new Map<number, number>();
  for (const p of byMetric[b] ?? []) bById.set(p.t, p.v);
  const timestamps: number[] = [];
  const values: number[] = [];
  for (const p of byMetric[a] ?? []) {
    const vb = bById.get(p.t);
    if (vb == null) continue;
    const { pct } = deriveMem(mem.id, { [a]: p.v, [b]: vb }, mem.totalUnitToBytes);
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
