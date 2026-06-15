// Routing & notifications (Alerts ▸ Routing & notifications). Two things: notification
// CHANNELS (where alerts can go — webhook/email; the connection config is a secret, sealed
// server-side and never returned) and routing RULES (which alerts, by severity, fan out to
// which channels). The notifier snapshots these (refreshed ~30s) so edits take effect live;
// any env-configured channel stays an always-on default route.
//
// Data-table standard v2: each list is a section header + toolbar (count + "+ Add …") over the
// shared `.ytable`. Add via modal; enable/disable is an inline icon toggle; delete confirms in a
// modal. Channel kind and rule severity are neutral/status chips (categorical vs status).

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  ChannelConfigInput,
  ChannelKind,
  NotificationChannel,
  RoutingRule,
  Severity,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon, PowerIcon } from '../components/ui/icons';
import './RoutingPage.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const SEVERITY_TONE: Record<Severity, 'critical' | 'warning' | 'neutral'> = {
  critical: 'critical',
  warning: 'warning',
  info: 'neutral',
};

const CHANNEL_COLS = '1.6fr 140px 130px 96px';
const RULE_COLS = '1.4fr 130px 1fr 130px 96px';

/** Inline status (dot + label) shared by channels and rules. */
function EnabledStatus({ enabled }: { enabled: boolean }) {
  return (
    <span className={enabled ? 'yt-status enabled' : 'yt-status disabled'}>
      <span className="yt-status-dot" />
      {enabled ? 'Enabled' : 'Disabled'}
    </span>
  );
}

export function RoutingPage() {
  const authed = useAuthStore((s) => s.authed);
  const [channels, setChannels] = useState<NotificationChannel[]>([]);
  const [rules, setRules] = useState<RoutingRule[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    Promise.all([api.listNotificationChannels(), api.listRoutingRules()])
      .then(([ch, ru]) => {
        setChannels(ch);
        setRules(ru);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  if (unavailable) {
    return (
      <div>
        <PageHeader
          title="Routing & notifications"
          trail={[{ label: 'Alerts' }, { label: 'Routing & notifications' }]}
        />
        <Card>
          <p className="muted">Notification routing is unavailable in skeleton mode.</p>
        </Card>
      </div>
    );
  }

  return (
    <div>
      <PageHeader
        title="Routing & notifications"
        trail={[{ label: 'Alerts' }, { label: 'Routing & notifications' }]}
        note="Channels are where alerts go; rules pick which alerts (by severity) reach which channels."
      />
      {error && <p className="form-error routing-error">{error}</p>}
      <ChannelsSection
        channels={channels}
        authed={authed}
        loading={loading}
        onChange={load}
        onError={setError}
      />
      <RulesSection
        rules={rules}
        channels={channels}
        authed={authed}
        loading={loading}
        onChange={load}
        onError={setError}
      />
    </div>
  );
}

// ── Channels ─────────────────────────────────────────────────────────────────

function ChannelsSection({
  channels,
  authed,
  loading,
  onChange,
  onError,
}: {
  channels: NotificationChannel[];
  authed: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<NotificationChannel | null>(null);

  const toggle = (c: NotificationChannel) =>
    api
      .setNotificationChannelEnabled(c.id, !c.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, 'failed to update')));

  return (
    <section>
      <div className="table-toolbar">
        <h2 className="table-section-title">Notification channels</h2>
        <TableSpacer />
        <ResultCount shown={channels.length} noun="channels" />
        {authed && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            + Add channel
          </Button>
        )}
      </div>

      <div className="ytable channels-table">
        <div className="ytable-head" style={{ gridTemplateColumns: CHANNEL_COLS }}>
          <div className="ytable-h">Name</div>
          <div className="ytable-h">Kind</div>
          <div className="ytable-h">Status</div>
          <div className="ytable-h right">Actions</div>
        </div>
        {channels.length === 0 ? (
          <div className="yt-empty">
            <p className="yt-empty-title">{loading ? 'Loading…' : 'No channels yet'}</p>
            {!loading && <p className="yt-empty-sub">Add a webhook or email destination.</p>}
          </div>
        ) : (
          channels.map((c) => (
            <div
              className={c.enabled ? 'ytable-row' : 'ytable-row is-muted'}
              style={{ gridTemplateColumns: CHANNEL_COLS }}
              key={c.id}
            >
              <div className="ytable-cell">
                <span className="yt-name-txt">{c.name}</span>
              </div>
              <div className="ytable-cell">
                <Badge tone="neutral">{c.kind}</Badge>
              </div>
              <div className="ytable-cell">
                <EnabledStatus enabled={c.enabled} />
              </div>
              <div className="ytable-cell right">
                {authed && (
                  <span className="ytable-actions">
                    <IconButton
                      title={c.enabled ? 'Disable channel' : 'Enable channel'}
                      onClick={() => toggle(c)}
                    >
                      <PowerIcon />
                    </IconButton>
                    <IconButton title="Delete channel" danger onClick={() => setDeleting(c)}>
                      <TrashIcon />
                    </IconButton>
                  </span>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {adding && (
        <AddChannelModal
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            onChange();
          }}
          onError={onError}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title="Delete channel"
          body={
            <>
              Delete channel <strong>{deleting.name}</strong>? Rules that target it will no longer
              reach this destination.
            </>
          }
          run={() => api.deleteNotificationChannel(deleting.id)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChange();
          }}
        />
      )}
    </section>
  );
}

function AddChannelModal({
  onClose,
  onDone,
  onError,
}: {
  onClose: () => void;
  onDone: () => void;
  onError: (m: string) => void;
}) {
  const [name, setName] = useState('');
  const [kind, setKind] = useState<ChannelKind>('webhook');
  const [url, setUrl] = useState('');
  const [host, setHost] = useState('');
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const [busy, setBusy] = useState(false);

  const canAdd =
    name.trim() !== '' &&
    (kind === 'webhook'
      ? url.trim() !== ''
      : host.trim() !== '' && from.trim() !== '' && to.trim() !== '');

  const submit = () => {
    if (!canAdd) return;
    setBusy(true);
    const config: ChannelConfigInput =
      kind === 'webhook'
        ? { kind: 'webhook', url: url.trim() }
        : { kind: 'email', host: host.trim(), from: from.trim(), to: to.trim() };
    api
      .createNotificationChannel({ name: name.trim(), config })
      .then(onDone)
      .catch((e: unknown) => {
        onError(errMsg(e, 'failed to add channel'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add notification channel"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canAdd || busy}>
            Add channel
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Kind</label>
        <Select value={kind} onChange={(e) => setKind(e.target.value as ChannelKind)}>
          <option value="webhook">webhook</option>
          <option value="email">email</option>
        </Select>
      </div>
      {kind === 'webhook' ? (
        <div className="modal-field">
          <label className="modal-field-label">Webhook URL</label>
          <TextInput
            className="mono"
            placeholder="https://hooks.example/…"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
          />
          <span className="modal-hint">The URL is sealed server-side and never returned.</span>
        </div>
      ) : (
        <>
          <div className="modal-field">
            <label className="modal-field-label">SMTP host</label>
            <TextInput value={host} onChange={(e) => setHost(e.target.value)} />
          </div>
          <div className="modal-field">
            <label className="modal-field-label">From</label>
            <TextInput value={from} onChange={(e) => setFrom(e.target.value)} />
          </div>
          <div className="modal-field">
            <label className="modal-field-label">To</label>
            <TextInput value={to} onChange={(e) => setTo(e.target.value)} />
          </div>
        </>
      )}
    </Modal>
  );
}

// ── Rules ────────────────────────────────────────────────────────────────────

function RulesSection({
  rules,
  channels,
  authed,
  loading,
  onChange,
  onError,
}: {
  rules: RoutingRule[];
  channels: NotificationChannel[];
  authed: boolean;
  loading: boolean;
  onChange: () => void;
  onError: (m: string) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<RoutingRule | null>(null);

  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;

  const toggle = (r: RoutingRule) =>
    api
      .setRoutingRuleEnabled(r.id, !r.enabled)
      .then(onChange)
      .catch((e: unknown) => onError(errMsg(e, 'failed to update')));

  return (
    <section className="routing-rules-section">
      <div className="table-toolbar">
        <h2 className="table-section-title">Routing rules</h2>
        <TableSpacer />
        <ResultCount shown={rules.length} noun="rules" />
        {authed && (
          <Button variant="primary" onClick={() => setAdding(true)} disabled={channels.length === 0}>
            + Add rule
          </Button>
        )}
      </div>

      <div className="ytable rules-table">
        <div className="ytable-head" style={{ gridTemplateColumns: RULE_COLS }}>
          <div className="ytable-h">Name</div>
          <div className="ytable-h">Severity</div>
          <div className="ytable-h">Channels</div>
          <div className="ytable-h">Status</div>
          <div className="ytable-h right">Actions</div>
        </div>
        {rules.length === 0 ? (
          <div className="yt-empty">
            <p className="yt-empty-title">{loading ? 'Loading…' : 'No routing rules'}</p>
            {!loading && (
              <p className="yt-empty-sub">
                Without a rule, alerts only use the env default route.
              </p>
            )}
          </div>
        ) : (
          rules.map((r) => (
            <div
              className={r.enabled ? 'ytable-row' : 'ytable-row is-muted'}
              style={{ gridTemplateColumns: RULE_COLS }}
              key={r.id}
            >
              <div className="ytable-cell">
                <span className="yt-name-txt">{r.name}</span>
              </div>
              <div className="ytable-cell">
                <Badge tone={r.severity ? SEVERITY_TONE[r.severity] : 'neutral'}>
                  {r.severity ?? 'any'}
                </Badge>
              </div>
              <div className="ytable-cell ellipsis">
                <span className="muted">
                  {r.channel_ids.map(channelName).join(', ') || '(no channels)'}
                </span>
              </div>
              <div className="ytable-cell">
                <EnabledStatus enabled={r.enabled} />
              </div>
              <div className="ytable-cell right">
                {authed && (
                  <span className="ytable-actions">
                    <IconButton
                      title={r.enabled ? 'Disable rule' : 'Enable rule'}
                      onClick={() => toggle(r)}
                    >
                      <PowerIcon />
                    </IconButton>
                    <IconButton title="Delete rule" danger onClick={() => setDeleting(r)}>
                      <TrashIcon />
                    </IconButton>
                  </span>
                )}
              </div>
            </div>
          ))
        )}
      </div>

      {adding && (
        <AddRuleModal
          channels={channels}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            onChange();
          }}
          onError={onError}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title="Delete routing rule"
          body={
            <>
              Delete rule <strong>{deleting.name}</strong>? Matching alerts will fall back to the
              env default route.
            </>
          }
          run={() => api.deleteRoutingRule(deleting.id)}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChange();
          }}
        />
      )}
    </section>
  );
}

function AddRuleModal({
  channels,
  onClose,
  onDone,
  onError,
}: {
  channels: NotificationChannel[];
  onClose: () => void;
  onDone: () => void;
  onError: (m: string) => void;
}) {
  const [name, setName] = useState('');
  const [severity, setSeverity] = useState<'' | Severity>('');
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);

  const toggle = (id: string) =>
    setSelected((cur) => {
      const next = new Set(cur);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const canAdd = name.trim() !== '' && selected.size > 0;

  const submit = () => {
    if (!canAdd) return;
    setBusy(true);
    api
      .createRoutingRule({
        name: name.trim(),
        severity: severity === '' ? null : severity,
        channel_ids: [...selected],
      })
      .then(onDone)
      .catch((e: unknown) => {
        onError(errMsg(e, 'failed to add rule'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add routing rule"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canAdd || busy}>
            Add rule
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Severity</label>
        <Select value={severity} onChange={(e) => setSeverity(e.target.value as '' | Severity)}>
          <option value="">any severity</option>
          <option value="critical">critical</option>
          <option value="warning">warning</option>
          <option value="info">info</option>
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Channels</label>
        <div className="routing-picks">
          {channels.map((c) => (
            <label key={c.id} className="routing-pick">
              <input type="checkbox" checked={selected.has(c.id)} onChange={() => toggle(c.id)} />
              {c.name}
            </label>
          ))}
        </div>
      </div>
    </Modal>
  );
}

// ── Shared confirm-delete modal ────────────────────────────────────────────────

function ConfirmDeleteModal({
  title,
  body,
  run,
  onClose,
  onDone,
}: {
  title: string;
  body: ReactNode;
  run: () => Promise<void>;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    run()
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={title}
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
      <p className="modal-confirm-text">{body}</p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
