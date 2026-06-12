// Maintenance windows (Alerts ▸ Maintenance windows). A window covers nodes by scope
// (node / profile / group, like thresholds) for a time range: covered nodes observe
// `maintenance` — no alerts fire and existing ones resolve — until the window ends.
// The engine refreshes its snapshot every ~30s, so boundaries take effect within that.

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
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import './CrudList.css';
import './MaintenancePage.css';

const LEVELS: ScopeLevel[] = ['node', 'profile', 'group'];

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

export function MaintenancePage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<MaintenanceWindow[]>([]);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [name, setName] = useState('');
  const [level, setLevel] = useState<ScopeLevel>('node');
  const [scopeId, setScopeId] = useState('');
  const [startsAt, setStartsAt] = useState('');
  const [endsAt, setEndsAt] = useState('');

  const load = useCallback(() => {
    api
      .listMaintenanceWindows()
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
    api.listProfiles().then(setProfiles).catch(() => undefined);
  }, [load]);

  const add = () => {
    setError(null);
    if (!startsAt || !endsAt) {
      setError('Start and end times are required.');
      return;
    }
    api
      .createMaintenanceWindow({
        name: name.trim(),
        scope_level: level,
        scope_id: scopeId.trim(),
        starts_at: toRfc3339(startsAt),
        ends_at: toRfc3339(endsAt),
      })
      .then(() => {
        setName('');
        setScopeId('');
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to add window')));
  };

  const setEnabled = (id: string, enabled: boolean) =>
    api
      .setMaintenanceWindowEnabled(id, enabled)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to update window')));

  const remove = (id: string) =>
    api
      .deleteMaintenanceWindow(id)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to delete window')));

  // Human label for a scope id (node/profile names resolved when known).
  const scopeLabel = (w: MaintenanceWindow): string => {
    if (w.level === 'node') return nodes.find((n) => n.id === w.scope_id)?.name ?? w.scope_id;
    if (w.level === 'profile')
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
        <Card title="Windows">
          {authed && (
            <div className="maint-form form-row crud-add">
              <TextInput
                placeholder="name (e.g. core switch firmware)"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
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
              <label className="maint-time">
                <span className="maint-time-label">from</span>
                <TextInput
                  type="datetime-local"
                  value={startsAt}
                  onChange={(e) => setStartsAt(e.target.value)}
                />
              </label>
              <label className="maint-time">
                <span className="maint-time-label">to</span>
                <TextInput
                  type="datetime-local"
                  value={endsAt}
                  onChange={(e) => setEndsAt(e.target.value)}
                />
              </label>
              <Button
                variant="primary"
                onClick={add}
                disabled={!name.trim() || !scopeId.trim() || !startsAt || !endsAt}
              >
                Add window
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}

          {rows.length === 0 ? (
            <p className="muted">No maintenance windows.</p>
          ) : (
            <div className="crud-list">
              {rows.map((w) => {
                const status = windowStatus(w);
                return (
                  <div className="crud-row maint-row" key={w.id}>
                    <Badge tone={status.tone}>{status.label}</Badge>
                    <span className="crud-name">{w.name}</span>
                    <span className="maint-scope">
                      <Badge tone="neutral">{w.level}</Badge>
                      <span className="maint-scope-name">{scopeLabel(w)}</span>
                    </span>
                    <span className="muted maint-range mono">
                      {fmtTime(w.starts_at)} → {fmtTime(w.ends_at)}
                    </span>
                    {authed && (
                      <>
                        <Button variant="ghost" onClick={() => setEnabled(w.id, !w.enabled)}>
                          {w.enabled ? 'Disable' : 'Enable'}
                        </Button>
                        <Button variant="ghost" onClick={() => remove(w.id)}>
                          Delete
                        </Button>
                      </>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
