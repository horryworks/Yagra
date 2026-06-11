// Node detail. Reached by drilling from All nodes (breadcrumb trail). Page-internal sub-tabs:
// Overview (live status, latest RTT, RTT history, a System (SNMP) scalar card, active alerts),
// Interfaces (per-interface traffic + utilization from SNMP table walks, joined with query-time
// rate()), and Collection (per-node override of what SNMP metrics to poll). Admins can edit the
// node's bindings (device profile + SNMP credential) from the header.

import { useEffect, useState } from 'react';
import { useParams } from 'react-router-dom';
import {
  formatBps,
  formatRtt,
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
  MetricReading,
  NodeState,
  NodeStatus,
  ProfileSummary,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Tabs } from '../components/ui/Tabs';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { Select } from '../components/ui/Field';
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

export function NodeDetailPage() {
  const { nodeId = '' } = useParams();
  const authed = useAuthStore((s) => s.authed);
  const [tab, setTab] = useState('overview');
  const [status, setStatus] = useState<NodeStatus | null>(null);
  const [reading, setReading] = useState<MetricReading | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [error, setError] = useState<string | null>(null);
  const [editingBindings, setEditingBindings] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setReading(null);
    setError(null);
    setStatus(null);
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
        title={<span className="mono">{nodeId}</span>}
        trail={[{ label: 'Nodes' }, { label: 'All nodes', to: '/nodes' }, { label: nodeId }]}
        actions={
          <div className="nodedetail-head-actions">
            {status && <StatusDot state={status.state} />}
            {authed && (
              <Button variant="outline" onClick={() => setEditingBindings(true)}>
                Edit bindings
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
    </div>
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

/** Per-interface traffic table (SNMP table walk + query-time rate utilization). Static (not
 *  virtualized): a node has dozens of interfaces, not thousands. Refreshes on an interval. */
function InterfacesTab({ nodeId }: { nodeId: string }) {
  const [rows, setRows] = useState<InterfaceRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);

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
        })
        .catch((e: unknown) => !cancelled && setError(errMsg(e, 'failed to load interfaces')));
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId]);

  return (
    <Card title="Interfaces">
      {error && <p className="form-error">{error}</p>}
      {loaded && rows.length === 0 ? (
        <p className="muted">
          No interfaces discovered yet. They appear after an SNMP table walk runs — the node
          needs a bound SNMP credential and a table metric in its collection set.
        </p>
      ) : (
        <div className="iftbl">
          <div className="iftbl-head">
            <div className="iftbl-h" />
            <div className="iftbl-h">Interface</div>
            <div className="iftbl-h">Alias</div>
            <div className="iftbl-h right">In</div>
            <div className="iftbl-h right">Out</div>
            <div className="iftbl-h right">In&nbsp;%</div>
            <div className="iftbl-h right">Out&nbsp;%</div>
            <div className="iftbl-h right">Speed</div>
          </div>
          {rows.map((r) => (
            <div className={r.stale ? 'iftbl-row stale' : 'iftbl-row'} key={r.ifindex}>
              <StatusDot state={operState(r.oper_status)} />
              <span className="iftbl-name mono">{r.if_name ?? `if${r.ifindex}`}</span>
              <span className="iftbl-alias">{r.if_alias ?? '—'}</span>
              <span className="iftbl-num">{formatBps(r.in_bps)}</span>
              <span className="iftbl-num">{formatBps(r.out_bps)}</span>
              <span className="iftbl-num">
                <UtilBadge pct={r.in_util_pct} />
              </span>
              <span className="iftbl-num">
                <UtilBadge pct={r.out_util_pct} />
              </span>
              <span className="iftbl-num">{formatBps(r.if_speed_bps)}</span>
            </div>
          ))}
        </div>
      )}
    </Card>
  );
}

/** Edit a node's bindings (device profile + SNMP credential). Pre-fills the current values
 *  from the node detail; saving replaces both via the bindings endpoint. */
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .getNode(nodeId)
      .then((n) => {
        setProfileId(n.profile_id ?? '');
        setCredentialId(n.credential_id ?? '');
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
      })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to save bindings'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Edit bindings"
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
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
