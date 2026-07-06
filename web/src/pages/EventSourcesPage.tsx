import { useCallback, useEffect, useMemo, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { EventSource } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon, PowerIcon, KeyIcon } from '../components/ui/icons';
import './EventSourcesPage.css';

const COLS = '1.6fr 120px 110px 130px';
const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function EventSourcesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<EventSource[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<EventSource | null>(null);
  const [deleting, setDeleting] = useState<EventSource | null>(null);
  // The one-time token disclosure after create / rotate.
  const [issued, setIssued] = useState<{ id: string; token: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .listEventSources()
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
  }, [load]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) => r.name.toLowerCase().includes(q));
  }, [rows, query]);

  const toggleEnabled = (r: EventSource) => {
    setError(null);
    api
      .updateEventSource(r.id, { name: r.name, enabled: !r.enabled, node_id: r.node_id })
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to update source')));
  };

  const rotate = (r: EventSource) => {
    setError(null);
    api
      .rotateEventSourceToken(r.id)
      .then(({ token }) => setIssued({ id: r.id, token }))
      .catch((e: unknown) => setError(errMsg(e, 'failed to rotate token')));
  };

  return (
    <div>
      <PageHeader
        title="Event sources"
        note="Webhook senders that POST events to Yagra. Each carries a bearer token, shown once at create/rotate; store it in the sender's config."
      />
      {unavailable ? (
        <Card>Event sources are unavailable in this mode (no metadata store).</Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search sources…"
              ariaLabel="Search event sources"
            />
            <TableSpacer />
            <ResultCount shown={filtered.length} total={rows.length} noun="sources" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add source
              </Button>
            )}
          </TableToolbar>
          {error && <p className="form-error">{error}</p>}
          <div className="ytable eventsources-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Name</div>
              <div className="ytable-h">Kind</div>
              <div className="ytable-h">Status</div>
              <div className="ytable-h right">Actions</div>
            </div>
            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading ? 'Loading…' : rows.length === 0 ? 'No event sources' : 'No sources match'}
                </p>
                {!loading && rows.length === 0 && (
                  <p className="yt-empty-sub">Add a webhook source to receive events over HTTP.</p>
                )}
              </div>
            ) : (
              filtered.map((r) => (
                <div className="ytable-row" key={r.id} style={{ gridTemplateColumns: COLS }}>
                  <div className="ytable-cell">{r.name}</div>
                  <div className="ytable-cell">
                    <Badge tone="neutral">{r.kind}</Badge>
                  </div>
                  <div className="ytable-cell">
                    <Badge tone={r.enabled ? 'up' : 'neutral'}>
                      {r.enabled ? 'enabled' : 'disabled'}
                    </Badge>
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <IconButton title="Rotate token" onClick={() => rotate(r)}>
                          <KeyIcon />
                        </IconButton>
                        <IconButton
                          title={r.enabled ? 'Disable source' : 'Enable source'}
                          onClick={() => toggleEnabled(r)}
                        >
                          <PowerIcon />
                        </IconButton>
                        <IconButton title="Edit source" onClick={() => setEditing(r)}>
                          <EditIcon />
                        </IconButton>
                        <IconButton title="Delete source" danger onClick={() => setDeleting(r)}>
                          <TrashIcon />
                        </IconButton>
                      </span>
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </>
      )}
      {adding && (
        <AddSourceModal
          onClose={() => setAdding(false)}
          onDone={(created) => {
            setAdding(false);
            setIssued(created);
            load();
          }}
        />
      )}
      {editing && (
        <EditSourceModal
          source={editing}
          onClose={() => setEditing(null)}
          onDone={() => {
            setEditing(null);
            load();
          }}
        />
      )}
      {deleting && (
        <DeleteSourceModal
          source={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
      {issued && <TokenModal issued={issued} onClose={() => setIssued(null)} />}
    </div>
  );
}

function AddSourceModal({
  onClose,
  onDone,
}: {
  onClose: () => void;
  onDone: (created: { id: string; token: string }) => void;
}) {
  const [name, setName] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const valid = name.trim() !== '';
  const submit = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    api
      .createEventSource({ name: name.trim() })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add source'));
        setBusy(false);
      });
  };
  return (
    <Modal
      title="Add webhook source"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            Create
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. Grafana alerts"
          autoFocus
        />
        <span className="modal-hint">A bearer token is generated and shown once after create.</span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

function EditSourceModal({
  source,
  onClose,
  onDone,
}: {
  source: EventSource;
  onClose: () => void;
  onDone: () => void;
}) {
  const [name, setName] = useState(source.name);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const valid = name.trim() !== '';
  const submit = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    api
      .updateEventSource(source.id, { name: name.trim(), enabled: source.enabled, node_id: source.node_id })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to save source'));
        setBusy(false);
      });
  };
  return (
    <Modal
      title="Edit webhook source"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

function DeleteSourceModal({
  source,
  onClose,
  onDone,
}: {
  source: EventSource;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteEventSource(source.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete source'));
        setBusy(false);
      });
  };
  return (
    <Modal
      title="Delete webhook source"
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
        Delete source <strong>{source.name}</strong>? Its token stops working immediately and rules
        scoped to it are removed.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

function TokenModal({
  issued,
  onClose,
}: {
  issued: { id: string; token: string };
  onClose: () => void;
}) {
  const [copiedToken, setCopiedToken] = useState(false);
  const [copiedUrl, setCopiedUrl] = useState(false);
  const url = `${window.location.origin}/api/v1/ingest/webhook/${issued.id}`;

  const copy = (text: string, mark: (v: boolean) => void) => {
    void navigator.clipboard?.writeText(text);
    mark(true);
    setTimeout(() => mark(false), 1200);
  };

  return (
    <Modal
      title="Webhook token — shown once"
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          Done
        </Button>
      }
    >
      <p className="modal-confirm-text">
        Copy this token now — it is stored only as a hash and cannot be shown again. Send it as
        <span className="mono"> Authorization: Bearer &lt;token&gt;</span>.
      </p>
      <div className="modal-field">
        <label className="modal-field-label">Token</label>
        <div className="eventsources-copyrow">
          <code className="eventsources-token mono">{issued.token}</code>
          <Button variant="outline" onClick={() => copy(issued.token, setCopiedToken)}>
            {copiedToken ? 'Copied' : 'Copy'}
          </Button>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Ingest URL</label>
        <div className="eventsources-copyrow">
          <code className="eventsources-token mono">{url}</code>
          <Button variant="outline" onClick={() => copy(url, setCopiedUrl)}>
            {copiedUrl ? 'Copied' : 'Copy'}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
