// Node detail. Reached by drilling from All nodes (breadcrumb trail). Page-internal sub-tabs:
// Overview (live status, latest RTT, RTT history, a System (SNMP) scalar card, active alerts),
// Interfaces (per-interface traffic + utilization from SNMP table walks, joined with query-time
// rate()), and Collection (per-node override of what SNMP metrics to poll). Admins can edit the
// node (device profile + SNMP credential bindings, plus its maker/model) from the header. The
// title reads "name (address) (maker) (model)", with the maker/model parts shown only when set.

import { useEffect, useState } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router-dom';
import {
  formatBps,
  formatRtt,
  formatSi,
  formatTimestamp,
  formatUtil,
  pointsToSeries,
  severityColorVar,
  stateLabel,
} from '../lib/format';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  CredentialSummary,
  InterfaceRow,
  InterfaceSeries,
  MetricReading,
  NodeDetail,
  NodeState,
  NodeStatus,
  ProfileSummary,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Tabs } from '../components/ui/Tabs';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { Select, TextInput } from '../components/ui/Field';
import { StatusDot } from '../components/ui/StatusDot';
import { Badge } from '../components/ui/Badge';
import { MetricChart } from '../components/MetricChart/MetricChart';
import { CollectionEditor } from '../components/CollectionEditor/CollectionEditor';
import './NodeDetailPage.css';

const METRIC = 'icmp_rtt_ms';
const STATUS_REFRESH_MS = 15_000;
/** Scalars always probed for the System card, on top of any configured ones. */
const BUILTIN_SCALARS = ['snmp_sys_uptime_ticks'];

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const TABS = ['overview', 'interfaces', 'collection'];

export function NodeDetailPage() {
  const { nodeId = '' } = useParams();
  const navigate = useNavigate();
  const authed = useAuthStore((s) => s.authed);
  // Keep the active sub-tab in the URL (`?tab=…`) so a browser reload restores it instead
  // of snapping back to Overview.
  const [searchParams, setSearchParams] = useSearchParams();
  const tabParam = searchParams.get('tab') ?? '';
  const tab = TABS.includes(tabParam) ? tabParam : 'overview';
  const setTab = (next: string) => {
    const params = new URLSearchParams(searchParams);
    params.set('tab', next);
    setSearchParams(params, { replace: true });
  };
  const [node, setNode] = useState<NodeDetail | null>(null);
  const [status, setStatus] = useState<NodeStatus | null>(null);
  const [reading, setReading] = useState<MetricReading | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [error, setError] = useState<string | null>(null);
  const [editingBindings, setEditingBindings] = useState(false);
  const [deleting, setDeleting] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setReading(null);
    setError(null);
    setStatus(null);
    setNode(null);
    // Node detail (name + address) for the header; falls back to the UUID if unavailable.
    api
      .getNode(nodeId)
      .then((n) => !cancelled && setNode(n))
      .catch(() => undefined);
    api
      .getNodeMetric(nodeId, METRIC)
      .then((r) => !cancelled && setReading(r))
      .catch((e: unknown) => !cancelled && setError(errMsg(e, 'no reading')));
    api
      .getNodeMetricRange(nodeId, METRIC)
      .then((r) => !cancelled && setSeries(pointsToSeries(r.points)))
      .catch(() => undefined);
    const loadStatus = () =>
      api
        .getNodeStatus(nodeId)
        .then((s) => !cancelled && setStatus(s))
        .catch(() => undefined);
    loadStatus();
    const id = setInterval(loadStatus, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId]);

  return (
    <div>
      <PageHeader
        title={
          node ? (
            <span>
              {node.name} <span className="mono nodedetail-title-addr">({node.address})</span>
              {node.vendor && <span className="nodedetail-title-meta"> ({node.vendor})</span>}
              {node.model && (
                <span className="nodedetail-title-meta mono"> ({node.model})</span>
              )}
            </span>
          ) : (
            <span className="mono">{nodeId}</span>
          )
        }
        trail={[
          { label: 'Nodes' },
          { label: 'All nodes', to: '/nodes' },
          { label: node?.name ?? nodeId },
        ]}
        actions={
          <div className="nodedetail-head-actions">
            {status && <StatusDot state={status.state} />}
            {authed && (
              <Button variant="outline" onClick={() => setEditingBindings(true)}>
                Edit node
              </Button>
            )}
            {authed && (
              <Button variant="danger" onClick={() => setDeleting(true)}>
                Delete
              </Button>
            )}
          </div>
        }
      />

      <Tabs
        tabs={[
          { key: 'overview', label: 'Overview' },
          { key: 'interfaces', label: 'Interfaces' },
          { key: 'collection', label: 'Collection' },
        ]}
        active={tab}
        onChange={setTab}
      />

      <div className="nodedetail-body">
        {tab === 'overview' && (
          <div className="nodedetail-grid">
            <Card title="Latest reading">
              {error && <p className="muted">{error}</p>}
              {!error && !reading && <p className="muted">Loading…</p>}
              {reading && (
                <div className="nodedetail-reading">
                  <div className="nodedetail-metric">{reading.metric}</div>
                  <div className="nodedetail-value">
                    {reading.metric === METRIC ? formatRtt(reading.value) : reading.value}
                  </div>
                </div>
              )}
            </Card>
            <Card title="RTT history">
              {series.timestamps.length === 0 ? (
                <p className="muted">No history yet…</p>
              ) : (
                <MetricChart title={METRIC} timestamps={series.timestamps} values={series.values} />
              )}
            </Card>
            <SnmpScalarsCard nodeId={nodeId} />
            <DeviceHealthCard nodeId={nodeId} />
            <Card title="Active alerts" className="nodedetail-span2">
              {!status || status.alerts.length === 0 ? (
                <p className="muted">No active alerts on this node.</p>
              ) : (
                <div className="nodedetail-alerts">
                  {status.alerts.map((a) => (
                    <div className="nodedetail-alert" key={`${a.check}|${a.severity}`}>
                      <span
                        className="nodedetail-alert-dot"
                        style={{ background: severityColorVar(a.severity) }}
                      />
                      <span className="nodedetail-alert-state">{stateLabel(a.state)}</span>
                      {a.root_cause && (
                        <span className="muted mono nodedetail-alert-cause">
                          ← caused by {a.root_cause}
                        </span>
                      )}
                      {a.flapping && <span className="nodedetail-alert-flap">flapping</span>}
                      <span className="muted nodedetail-alert-time">
                        {formatTimestamp(a.at_unix_ms)}
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </Card>
          </div>
        )}

        {tab === 'interfaces' && <InterfacesTab nodeId={nodeId} />}

        {tab === 'collection' && (
          <Card title="Collection set — this node">
            <p className="muted nodedetail-collection-note">
              SNMP metrics polled from this node. Node-level entries override the device
              profile; with none set, the profile / built-in defaults apply.
            </p>
            <CollectionEditor scope="node" scopeId={nodeId} canEdit={authed} />
          </Card>
        )}
      </div>

      {editingBindings && (
        <BindingsModal
          nodeId={nodeId}
          onClose={() => setEditingBindings(false)}
          onDone={() => setEditingBindings(false)}
        />
      )}
      {deleting && (
        <DeleteNodeModal
          nodeId={nodeId}
          name={node?.name ?? nodeId}
          onClose={() => setDeleting(false)}
          onDeleted={() => navigate('/nodes')}
        />
      )}
    </div>
  );
}

/** Confirm + delete a node (destructive-consent modal). On success the caller navigates away. */
function DeleteNodeModal({
  nodeId,
  name,
  onClose,
  onDeleted,
}: {
  nodeId: string;
  name: string;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteNode(nodeId)
      .then(onDeleted)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete node'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete node"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Delete
          </Button>
        </>
      }
    >
      <p>
        Delete node <strong>{name}</strong>? This removes its inventory entry and its
        discovered interfaces. Collected metrics age out of the TSDB. This cannot be undone.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Latest values of the node's scalar SNMP metrics. Hidden entirely when the node has no
 *  SNMP scalar readings (e.g. an ICMP-only node), so it never shows an empty box. */
function SnmpScalarsCard({ nodeId }: { nodeId: string }) {
  const [readings, setReadings] = useState<{ name: string; value: number }[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const names = new Set(BUILTIN_SCALARS);
      // Extend with any configured scalars (admin-only endpoint; ignore if not permitted).
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
    <Card title="System (SNMP)">
      <div className="nodedetail-scalars">
        {readings.map((r) => (
          <div className="nodedetail-scalar" key={r.name}>
            <span className="nodedetail-scalar-name mono">{r.name}</span>
            <span className="nodedetail-scalar-value">{r.value}</span>
          </div>
        ))}
      </div>
    </Card>
  );
}

/** ifOperStatus (1 = up) → a node-state colour for the StatusDot. */
function operState(oper: number | null): NodeState {
  if (oper == null) return 'unknown';
  return oper === 1 ? 'ok' : 'critical';
}

function UtilBadge({ pct }: { pct: number | null }) {
  if (pct == null) return <span className="muted">—</span>;
  const tone = pct >= 90 ? 'critical' : pct >= 70 ? 'warning' : 'up';
  return <Badge tone={tone}>{formatUtil(pct)}</Badge>;
}

/** Chart time-windows for the interface detail pane. */
const RANGES: { label: string; secs: number }[] = [
  { label: '1h', secs: 3600 },
  { label: '6h', secs: 6 * 3600 },
  { label: '24h', secs: 24 * 3600 },
];
const CHART_IN = '#4f8cff';
const CHART_OUT = '#34d399';

/** Vendor health metrics that read as a 0–100 percentage, by role. (cisco_mem_used/free are
 *  bytes, and HOST-RESOURCES memory needs a used/size ratio — both out of scope here.) */
const HEALTH_METRICS: { metric: string; role: 'cpu' | 'mem' }[] = [
  { metric: 'huawei_cpu_usage', role: 'cpu' },
  { metric: 'cisco_cpu_5min', role: 'cpu' },
  { metric: 'hr_processor_load', role: 'cpu' },
  { metric: 'huawei_mem_usage', role: 'mem' },
];
const HEALTH_ROLE_LABEL: Record<'cpu' | 'mem', string> = { cpu: 'CPU', mem: 'Memory' };
const HEALTH_ROLES = ['cpu', 'mem'] as const;

/** Device CPU/Memory health: node-level percentages (a query-time `max()` across the per-entity
 *  table series) with a trend chart. The card resolves which health metrics the node actually
 *  has from its effective collection set, and hides entirely when it has none (e.g. an ICMP-only
 *  node, or a device without CPU/mem OIDs) — like the System (SNMP) card. */
function DeviceHealthCard({ nodeId }: { nodeId: string }) {
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
    <Card title="Device health" className="nodedetail-span2">
      <div className="devhealth-windows">
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
      <div className="devhealth-metrics">
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
    </Card>
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
          r.status === 'fulfilled'
            ? pointsToSeries(r.value.points)
            : { timestamps: [], values: [] },
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
    <div className="devhealth-metric">
      <div className="devhealth-metric-head">
        <span className="devhealth-metric-label">{label}</span>
        <span className="devhealth-metric-value">{formatUtil(value)}</span>
      </div>
      {series.timestamps.length > 0 ? (
        <MetricChart
          title=""
          timestamps={series.timestamps}
          values={series.values}
          yFormat={formatUtil}
        />
      ) : (
        <p className="muted">No history yet…</p>
      )}
    </div>
  );
}

/** Interfaces: master/detail — left = the interface list (status/name/util), right = charts
 *  for the selected interface. The list refreshes on an interval; the first row auto-selects. */
function InterfacesTab({ nodeId }: { nodeId: string }) {
  const [rows, setRows] = useState<InterfaceRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      api
        .listNodeInterfaces(nodeId)
        .then((r) => {
          if (cancelled) return;
          setRows(r);
          setError(null);
          setLoaded(true);
          // Auto-select the first interface so the detail pane isn't empty on arrival.
          setSelected((cur) => (cur == null && r.length > 0 ? r[0].ifindex : cur));
        })
        .catch((e: unknown) => !cancelled && setError(errMsg(e, 'failed to load interfaces')));
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId]);

  const selectedRow = rows.find((r) => r.ifindex === selected) ?? null;

  return (
    <Card title="Interfaces">
      {error && <p className="form-error">{error}</p>}
      {loaded && rows.length === 0 ? (
        <p className="muted">
          No interfaces discovered yet. They appear after an SNMP table walk runs — the node
          needs a bound SNMP credential and a table metric in its collection set.
        </p>
      ) : (
        <div className="iftab-split">
          <div className="iflist">
            <div className="iflist-head">
              <div className="iflist-h">Status</div>
              <div className="iflist-h">Interface</div>
              <div className="iflist-h right">In&nbsp;%</div>
              <div className="iflist-h right">Out&nbsp;%</div>
            </div>
            {rows.map((r) => (
              <button
                type="button"
                key={r.ifindex}
                className={`iflist-row${r.ifindex === selected ? ' selected' : ''}${
                  r.stale ? ' stale' : ''
                }`}
                onClick={() => setSelected(r.ifindex)}
              >
                <StatusDot state={operState(r.oper_status)} />
                <span className="iflist-name mono">{r.if_name ?? `if${r.ifindex}`}</span>
                <span className="iflist-num">
                  <UtilBadge pct={r.in_util_pct} />
                </span>
                <span className="iflist-num">
                  <UtilBadge pct={r.out_util_pct} />
                </span>
              </button>
            ))}
          </div>
          <div className="iftab-detail">
            {selectedRow ? (
              <InterfaceDetail nodeId={nodeId} row={selectedRow} />
            ) : (
              <p className="muted">Select an interface to see its charts.</p>
            )}
          </div>
        </div>
      )}
    </Card>
  );
}

/** Charts + stats for one interface: throughput (In/Out bps) and errors (In/Out per second)
 *  over a selectable window, plus the current stat tiles (from the list row). */
function InterfaceDetail({ nodeId, row }: { nodeId: string; row: InterfaceRow }) {
  const [rangeSecs, setRangeSecs] = useState(RANGES[0].secs);
  const [series, setSeries] = useState<InterfaceSeries | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const to = Math.floor(Date.now() / 1000);
      api
        .getInterfaceSeries(nodeId, row.ifindex, { from: to - rangeSecs, to })
        .then((s) => !cancelled && setSeries(s))
        .catch(() => undefined);
    };
    setSeries(null);
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId, row.ifindex, rangeSecs]);

  const ts = series?.timestamps ?? [];
  const hasData = ts.length > 0;

  return (
    <div className="ifdetail">
      <div className="ifdetail-head">
        <StatusDot state={operState(row.oper_status)} />
        <span className="mono ifdetail-name">{row.if_name ?? `if${row.ifindex}`}</span>
        {row.if_alias && <span className="muted ifdetail-alias">{row.if_alias}</span>}
        <div className="ifdetail-windows">
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

      <div className="ifdetail-section">
        <div className="ifdetail-section-label">
          Throughput (In / Out, <span className="ifdetail-unit">bps</span>)
        </div>
        {hasData ? (
          <MetricChart
            title=""
            timestamps={ts}
            yFormat={formatSi}
            series={[
              { label: 'In', values: series!.in_bps, color: CHART_IN },
              { label: 'Out', values: series!.out_bps, color: CHART_OUT },
            ]}
          />
        ) : (
          <p className="muted">No data for this window yet…</p>
        )}
      </div>

      <div className="ifdetail-section">
        <div className="ifdetail-section-label">
          Errors (In / Out, <span className="ifdetail-unit">/s</span>)
        </div>
        {hasData ? (
          <MetricChart
            title=""
            timestamps={ts}
            yFormat={formatSi}
            series={[
              { label: 'In', values: series!.in_errors, color: CHART_IN },
              { label: 'Out', values: series!.out_errors, color: CHART_OUT },
            ]}
          />
        ) : (
          <p className="muted">No data for this window yet…</p>
        )}
      </div>

      <div className="ifdetail-stats">
        <div className="ifdetail-stat">
          <span className="muted">In</span> {formatBps(row.in_bps)}
        </div>
        <div className="ifdetail-stat">
          <span className="muted">Out</span> {formatBps(row.out_bps)}
        </div>
        <div className="ifdetail-stat">
          <span className="muted">In %</span> {formatUtil(row.in_util_pct)}
        </div>
        <div className="ifdetail-stat">
          <span className="muted">Out %</span> {formatUtil(row.out_util_pct)}
        </div>
        <div className="ifdetail-stat">
          <span className="muted">Speed</span> {formatBps(row.if_speed_bps)}
        </div>
      </div>
    </div>
  );
}

/** Edit a node: device profile + SNMP credential bindings and its descriptive maker/model.
 *  Pre-fills the current values from the node detail; saving sends all of them via the bindings
 *  endpoint (so an unchanged field is preserved, not cleared). */
function BindingsModal({
  nodeId,
  onClose,
  onDone,
}: {
  nodeId: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  const [profileId, setProfileId] = useState('');
  const [credentialId, setCredentialId] = useState('');
  const [vendor, setVendor] = useState('');
  const [model, setModel] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .getNode(nodeId)
      .then((n) => {
        setProfileId(n.profile_id ?? '');
        setCredentialId(n.credential_id ?? '');
        setVendor(n.vendor ?? '');
        setModel(n.model ?? '');
      })
      .catch(() => undefined);
    api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    api
      .listCredentials()
      .then((c) => setCredentials(c.filter((cr) => cr.kind === 'snmp_v2c')))
      .catch(() => setCredentials([]));
  }, [nodeId]);

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setNodeBindings(nodeId, {
        profile_id: profileId || null,
        credential_id: credentialId || null,
        vendor: vendor.trim() || null,
        model: model.trim() || null,
      })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to save node'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Edit node"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          Device profile
          <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
            <option value="">— none —</option>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          SNMP credential
          <Select value={credentialId} onChange={(e) => setCredentialId(e.target.value)}>
            <option value="">— none —</option>
            {credentials.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
        </label>
        <div className="form-row">
          <label className="form-label">
            Maker
            <TextInput
              value={vendor}
              onChange={(e) => setVendor(e.target.value)}
              placeholder="e.g. Cisco"
            />
          </label>
          <label className="form-label">
            Model
            <TextInput
              className="mono"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder="e.g. C2960"
            />
          </label>
        </div>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
