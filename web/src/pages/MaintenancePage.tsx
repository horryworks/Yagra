// Maintenance windows (Alerts ▸ Maintenance windows). A window covers nodes by scope
// (node / profile / group, like thresholds) for a time range: covered nodes observe
// `maintenance` — no alerts fire and existing ones resolve — until the window ends.
// The engine refreshes its snapshot every ~30s, so boundaries take effect within that.
//
// Data-table standard v2: a toolbar (count + "+ Add window") over the shared `.ytable`.
// Add and delete both go through modals; enable/disable is an immediate row action.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  MaintenanceWindow,
  NodeSummary,
  ProfileSummary,
  ScopeLevel,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { PowerIcon, TrashIcon } from '../components/ui/icons';
import { localTimeZone } from '../lib/format';
import './MaintenancePage.css';

const LEVELS: ScopeLevel[] = ['node', 'profile', 'group'];
const TZ = localTimeZone();

const COLS = '120px 1.4fr 1fr 230px 120px';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

/** datetime-local input value → RFC 3339 (UTC) for the API. */
const toRfc3339 = (local: string) => new Date(local).toISOString();

/** RFC 3339 → compact local display. */
const fmtTime = (iso: string) =>
  new Date(iso).toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });

function windowStatus(w: MaintenanceWindow): { label: string; tone: 'info' | 'neutral' } {
  if (!w.enabled) return { label: 'disabled', tone: 'neutral' };
  if (w.active) return { label: 'active', tone: 'info' };
  if (new Date(w.ends_at).getTime() < Date.now()) return { label: 'ended', tone: 'neutral' };
  return { label: 'scheduled', tone: 'neutral' };
}

/** Create a maintenance window (scope-driven focused-editing modal). */
function AddWindowModal({
  nodes,
  profiles,
  onClose,
  onSaved,
}: {
  nodes: NodeSummary[];
  profiles: ProfileSummary[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const [name, setName] = useState('');
  const [level, setLevel] = useState<ScopeLevel>('node');
  const [scopeId, setScopeId] = useState('');
  const [startsAt, setStartsAt] = useState('');
  const [endsAt, setEndsAt] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const ready = !(!name.trim() || !scopeId.trim() || !startsAt || !endsAt);

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createMaintenanceWindow({
        name: name.trim(),
        scope_level: level,
        scope_id: scopeId.trim(),
        starts_at: toRfc3339(startsAt),
        ends_at: toRfc3339(endsAt),
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add window'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add maintenance window"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            Add window
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput
          placeholder="e.g. core switch firmware"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Scope level</label>
        <Select
          value={level}
          onChange={(e) => {
            setLevel(e.target.value as ScopeLevel);
            setScopeId('');
          }}
        >
          {LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Scope</label>
        {level === 'node' ? (
          <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
            <option value="">(pick a node)</option>
            {nodes.map((n) => (
              <option key={n.id} value={n.id}>
                {n.name}
              </option>
            ))}
          </Select>
        ) : level === 'profile' ? (
          <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
            <option value="">(pick a profile)</option>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </Select>
        ) : (
          <TextInput
            className="mono"
            placeholder="group (tag value)"
            value={scopeId}
            onChange={(e) => setScopeId(e.target.value)}
          />
        )}
        {level === 'group' && (
          <span className="modal-hint">
            Scope “group” matches nodes by their group tag value (e.g. a site name).
          </span>
        )}
      </div>
      <div className="modal-field">
        <label className="modal-field-label">From</label>
        <TextInput
          type="datetime-local"
          value={startsAt}
          onChange={(e) => setStartsAt(e.target.value)}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">To</label>
        <TextInput
          type="datetime-local"
          value={endsAt}
          onChange={(e) => setEndsAt(e.target.value)}
        />
        <span className="modal-hint">
          “From”/“To” are in {TZ} (your browser’s local time) and are stored as UTC.
        </span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a maintenance window (destructive-consent modal). */
function DeleteWindowModal({
  win,
  onClose,
  onDone,
}: {
  win: MaintenanceWindow;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteMaintenanceWindow(win.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete window'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete maintenance window"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Delete
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        Delete maintenance window <strong>{win.name}</strong>? This cannot be undone.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function MaintenancePage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<MaintenanceWindow[]>([]);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<MaintenanceWindow | null>(null);

  const load = useCallback(() => {
    api
      .listMaintenanceWindows()
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
    api.listProfiles().then(setProfiles).catch(() => undefined);
  }, [load]);

  const setEnabled = (id: string, enabled: boolean) =>
    api
      .setMaintenanceWindowEnabled(id, enabled)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to update window')));

  // Human label for a scope id (node/profile names resolved when known).
  const scopeLabel = (w: MaintenanceWindow): string => {
    if (w.scope_level === 'node')
      return nodes.find((n) => n.id === w.scope_id)?.name ?? w.scope_id;
    if (w.scope_level === 'profile')
      return profiles.find((p) => p.id === w.scope_id)?.name ?? w.scope_id;
    return w.scope_id;
  };

  return (
    <div>
      <PageHeader
        title="Maintenance windows"
        trail={[{ label: 'Alerts' }, { label: 'Maintenance windows' }]}
        note="Planned work: covered nodes show maintenance and raise no alerts for the window."
      />

      {unavailable ? (
        <Card>
          <p className="muted">Maintenance management is unavailable in skeleton mode.</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={rows.length} noun="windows" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add window
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <div className="ytable">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Status</div>
              <div className="ytable-h">Name</div>
              <div className="ytable-h">Scope</div>
              <div className="ytable-h">Range</div>
              <div className="ytable-h right">Actions</div>
            </div>

            {rows.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading ? 'Loading…' : 'No maintenance windows'}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    Schedule planned work so covered nodes raise no alerts.
                  </p>
                )}
              </div>
            ) : (
              rows.map((w) => {
                const status = windowStatus(w);
                return (
                  <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={w.id}>
                    <div className="ytable-cell">
                      <Badge tone={status.tone}>{status.label}</Badge>
                    </div>
                    <div className="ytable-cell">
                      <span className="yt-name-txt">{w.name}</span>
                    </div>
                    <div className="ytable-cell">
                      <span className="maint-scope">
                        <Badge>{w.scope_level}</Badge>
                        <span>{scopeLabel(w)}</span>
                      </span>
                    </div>
                    <div className="ytable-cell mono">
                      {fmtTime(w.starts_at)} → {fmtTime(w.ends_at)}
                    </div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton
                            title={w.enabled ? 'Disable' : 'Enable'}
                            onClick={() => setEnabled(w.id, !w.enabled)}
                          >
                            <PowerIcon />
                          </IconButton>
                          <IconButton title="Delete" danger onClick={() => setDeleting(w)}>
                            <TrashIcon />
                          </IconButton>
                        </span>
                      )}
                    </div>
                  </div>
                );
              })
            )}
          </div>
        </>
      )}

      {adding && (
        <AddWindowModal
          nodes={nodes}
          profiles={profiles}
          onClose={() => setAdding(false)}
          onSaved={load}
        />
      )}
      {deleting && (
        <DeleteWindowModal
          win={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
    </div>
  );
}
