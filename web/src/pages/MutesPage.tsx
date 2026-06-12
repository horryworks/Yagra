// Mutes (Alerts ▸ Mutes). A mute silences notifications for one node — optionally one
// check (metric name) — until a given time. The alert still fires and shows in the UI and
// history; only the page is suppressed. Expired mutes drop off automatically.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Mute, NodeSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import './CrudList.css';
import './MutesPage.css';

// Check-name presets: the liveness check plus the common polled metrics.
const CHECK_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const toRfc3339 = (local: string) => new Date(local).toISOString();

const fmtTime = (iso: string) =>
  new Date(iso).toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });

export function MutesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<Mute[]>([]);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [nodeId, setNodeId] = useState('');
  const [check, setCheck] = useState('');
  const [until, setUntil] = useState('');
  const [reason, setReason] = useState('');

  const load = useCallback(() => {
    api
      .listMutes()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      });
  }, []);

  useEffect(() => {
    load();
    api.listNodes().then(setNodes).catch(() => undefined);
  }, [load]);

  const add = () => {
    setError(null);
    if (!until) {
      setError('An expiry time is required.');
      return;
    }
    api
      .createMute({
        node_id: nodeId,
        check: check.trim() || undefined,
        until: toRfc3339(until),
        reason: reason.trim() || undefined,
      })
      .then(() => {
        setCheck('');
        setReason('');
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to add mute')));
  };

  const remove = (id: string) =>
    api
      .deleteMute(id)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to lift mute')));

  const nodeName = (id: string) => nodes.find((n) => n.id === id)?.name ?? id;

  return (
    <div>
      <PageHeader
        title="Mutes"
        trail={[{ label: 'Alerts' }, { label: 'Mutes' }]}
        note="Silence notifications for a node (or one check) until a time — alerts stay visible."
      />

      {unavailable ? (
        <Card>
          <p className="muted">Mute management is unavailable in skeleton mode.</p>
        </Card>
      ) : (
        <Card title="Active mutes">
          {authed && (
            <div className="mutes-form form-row crud-add">
              <Select value={nodeId} onChange={(e) => setNodeId(e.target.value)}>
                <option value="">(pick a node)</option>
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </Select>
              <TextInput
                className="mono"
                placeholder="check (empty = whole node)"
                list="mute-check-presets"
                value={check}
                onChange={(e) => setCheck(e.target.value)}
              />
              <datalist id="mute-check-presets">
                {CHECK_PRESETS.map((c) => (
                  <option key={c} value={c} />
                ))}
              </datalist>
              <label className="mutes-until">
                <span className="mutes-until-label">until</span>
                <TextInput
                  type="datetime-local"
                  value={until}
                  onChange={(e) => setUntil(e.target.value)}
                />
              </label>
              <TextInput
                placeholder="reason (optional)"
                value={reason}
                onChange={(e) => setReason(e.target.value)}
              />
              <Button variant="primary" onClick={add} disabled={!nodeId || !until}>
                Mute
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}

          {rows.length === 0 ? (
            <p className="muted">No active mutes.</p>
          ) : (
            <div className="crud-list">
              {rows.map((m) => (
                <div className="crud-row mutes-row" key={m.id}>
                  <span className="crud-name">{nodeName(m.node_id)}</span>
                  {m.check_name ? (
                    <Badge tone="neutral">
                      <span className="mono">{m.check_name}</span>
                    </Badge>
                  ) : (
                    <Badge tone="info">all checks</Badge>
                  )}
                  <span className="muted mutes-until-at mono">until {fmtTime(m.until_at)}</span>
                  <span className="muted mutes-reason">{m.reason}</span>
                  {authed && (
                    <Button variant="ghost" onClick={() => remove(m.id)}>
                      Lift
                    </Button>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
