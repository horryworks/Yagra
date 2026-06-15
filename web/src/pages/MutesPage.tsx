// Mutes (Alerts ▸ Mutes). A mute silences notifications for one node — optionally one
// check (metric name) — until a given time. The alert still fires and shows in the UI and
// history; only the page is suppressed. Expired mutes drop off automatically.
//
// Data-table standard v2: a toolbar (count + "+ Add mute") over the shared `.ytable`. Adding a
// mute and lifting one both go through modals — the add form picks a node + optional check and an
// expiry; lifting is a destructive-consent confirm.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Mute, NodeSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import { localTimeZone } from '../lib/format';
import './MutesPage.css';

const TZ = localTimeZone();

// Check-name presets: the liveness check plus the common polled metrics.
const CHECK_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

const COLS = '1.4fr 170px 180px 1fr 92px';

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

/** Create a mute (focused-editing modal): node + optional check, expiry, optional reason. */
function AddMuteModal({
  nodes,
  onClose,
  onSaved,
}: {
  nodes: NodeSummary[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const [nodeId, setNodeId] = useState('');
  const [check, setCheck] = useState('');
  const [until, setUntil] = useState('');
  const [reason, setReason] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    if (!nodeId || !until) return;
    setBusy(true);
    setError(null);
    api
      .createMute({
        node_id: nodeId,
        check: check.trim() || undefined,
        until: toRfc3339(until),
        reason: reason.trim() || undefined,
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add mute'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add mute"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!nodeId || !until || busy}>
            Add mute
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Node</label>
        <Select value={nodeId} onChange={(e) => setNodeId(e.target.value)} autoFocus>
          <option value="">(pick a node)</option>
          {nodes.map((n) => (
            <option key={n.id} value={n.id}>
              {n.name}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Check</label>
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
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Until</label>
        <TextInput
          type="datetime-local"
          value={until}
          onChange={(e) => setUntil(e.target.value)}
        />
        <span className="modal-hint">
          In {TZ} (your browser’s local time) and stored as UTC.
        </span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Reason</label>
        <TextInput
          placeholder="reason (optional)"
          value={reason}
          onChange={(e) => setReason(e.target.value)}
        />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + lift a mute (destructive-consent modal). */
function LiftMuteModal({
  mute,
  nodeName,
  onClose,
  onDone,
}: {
  mute: Mute;
  nodeName: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteMute(mute.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to lift mute'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Lift mute"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Lift mute
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        Lift the mute on <strong>{nodeName}</strong>
        {mute.check_name ? (
          <>
            {' '}
            (<span className="mono">{mute.check_name}</span>)
          </>
        ) : (
          ' (all checks)'
        )}
        ? Notifications resume immediately.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function MutesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<Mute[]>([]);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [lifting, setLifting] = useState<Mute | null>(null);

  const load = useCallback(() => {
    api
      .listMutes()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
    api.listNodes().then(setNodes).catch(() => undefined);
  }, [load]);

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
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={rows.length} noun="active mutes" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add mute
              </Button>
            )}
          </TableToolbar>

          <div className="ytable">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h">Node</div>
                <div className="ytable-h">Check</div>
                <div className="ytable-h">Until</div>
                <div className="ytable-h">Reason</div>
                <div className="ytable-h right">Actions</div>
              </div>

              {rows.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">{loading ? 'Loading…' : 'No active mutes'}</p>
                  {!loading && (
                    <p className="yt-empty-sub">
                      Silence a node (or one check) until a time to suppress its notifications.
                    </p>
                  )}
                </div>
              ) : (
                rows.map((m) => (
                  <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={m.id}>
                    <div className="ytable-cell">
                      <span className="yt-name-txt">{nodeName(m.node_id)}</span>
                    </div>
                    <div className="ytable-cell">
                      {m.check_name ? (
                        <Badge tone="neutral">
                          <span className="mono">{m.check_name}</span>
                        </Badge>
                      ) : (
                        <Badge tone="info">all checks</Badge>
                      )}
                    </div>
                    <div className="ytable-cell mono">{fmtTime(m.until_at)}</div>
                    <div className="ytable-cell ellipsis muted">{m.reason}</div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton title="Lift mute" danger onClick={() => setLifting(m)}>
                            <TrashIcon />
                          </IconButton>
                        </span>
                      )}
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        </>
      )}

      {adding && (
        <AddMuteModal nodes={nodes} onClose={() => setAdding(false)} onSaved={load} />
      )}
      {lifting && (
        <LiftMuteModal
          mute={lifting}
          nodeName={nodeName(lifting.node_id)}
          onClose={() => setLifting(null)}
          onDone={() => {
            setLifting(null);
            load();
          }}
        />
      )}
    </div>
  );
}
