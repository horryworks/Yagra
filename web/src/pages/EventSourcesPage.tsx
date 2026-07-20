// SPDX-License-Identifier: AGPL-3.0-only
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { EventSource } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon, PowerIcon, KeyIcon } from '../components/ui/icons';
import './EventSourcesPage.css';

const COLS = '1.6fr 120px 110px 130px';
const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function EventSourcesPage() {
  const { t } = useTranslation('alertsConfig');
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
      .catch((e: unknown) => setError(errMsg(e, t('eventSources.err.update'))));
  };

  const rotate = (r: EventSource) => {
    setError(null);
    api
      .rotateEventSourceToken(r.id)
      .then(({ token }) => setIssued({ id: r.id, token }))
      .catch((e: unknown) => setError(errMsg(e, t('eventSources.err.rotate'))));
  };

  return (
    <div>
      <PageHeader title={t('nav:alerts.eventSources')} note={t('eventSources.note')} />
      {unavailable ? (
        <Card>{t('eventSources.unavailable')}</Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('eventSources.searchPlaceholder')}
              ariaLabel={t('eventSources.searchAria')}
            />
            <TableSpacer />
            <ResultCount
              shown={filtered.length}
              total={rows.length}
              noun={t('noun.source', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('eventSources.add')}
              </Button>
            )}
          </TableToolbar>
          {error && <p className="form-error">{error}</p>}
          <div className="ytable eventsources-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('eventSources.cols.name')}</div>
              <div className="ytable-h">{t('eventSources.cols.kind')}</div>
              <div className="ytable-h">{t('eventSources.cols.status')}</div>
              <div className="ytable-h right">{t('eventSources.cols.actions')}</div>
            </div>
            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? t('common:loading')
                    : rows.length === 0
                      ? t('eventSources.empty')
                      : t('eventSources.emptyMatch')}
                </p>
                {!loading && rows.length === 0 && (
                  <p className="yt-empty-sub">{t('eventSources.emptySub')}</p>
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
                      {r.enabled ? t('status.enabled') : t('status.disabled')}
                    </Badge>
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <OverflowMenu
                          actions={[
                            {
                              label: t('eventSources.rotate'),
                              icon: <KeyIcon />,
                              onClick: () => rotate(r),
                            },
                            {
                              label: r.enabled
                                ? t('eventSources.disable')
                                : t('eventSources.enable'),
                              icon: <PowerIcon />,
                              onClick: () => toggleEnabled(r),
                            },
                            {
                              label: t('eventSources.edit'),
                              icon: <EditIcon />,
                              onClick: () => setEditing(r),
                            },
                            {
                              label: t('eventSources.delete'),
                              icon: <TrashIcon />,
                              danger: true,
                              onClick: () => setDeleting(r),
                            },
                          ]}
                        />
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
  const { t } = useTranslation('alertsConfig');
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
        setError(errMsg(e, t('eventSources.err.add')));
        setBusy(false);
      });
  };
  return (
    <Modal
      title={t('eventSources.addModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {t('eventSources.addModal.create')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('eventSources.addModal.name')}</label>
        <TextInput
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder={t('eventSources.addModal.namePlaceholder')}
          autoFocus
        />
        <span className="modal-hint">{t('eventSources.addModal.hint')}</span>
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
  const { t } = useTranslation('alertsConfig');
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
        setError(errMsg(e, t('eventSources.err.save')));
        setBusy(false);
      });
  };
  return (
    <Modal
      title={t('eventSources.editModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('eventSources.editModal.name')}</label>
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
  const { t } = useTranslation('alertsConfig');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteEventSource(source.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('eventSources.err.delete')));
        setBusy(false);
      });
  };
  return (
    <Modal
      title={t('eventSources.deleteModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {t('common:actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="eventSources.deleteModal.body"
          values={{ name: source.name }}
          components={{ strong: <strong /> }}
        />
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
  const { t } = useTranslation('alertsConfig');
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
      title={t('eventSources.token.title')}
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('eventSources.token.done')}
        </Button>
      }
    >
      <p className="modal-confirm-text">
        {t('eventSources.token.sendAs')}
        <span className="mono"> Authorization: Bearer &lt;token&gt;</span>.
      </p>
      <div className="modal-field">
        <label className="modal-field-label">{t('eventSources.token.label')}</label>
        <div className="eventsources-copyrow">
          <code className="eventsources-token mono">{issued.token}</code>
          <Button variant="outline" onClick={() => copy(issued.token, setCopiedToken)}>
            {copiedToken ? t('common:copy.copied') : t('eventSources.token.copy')}
          </Button>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('eventSources.token.url')}</label>
        <div className="eventsources-copyrow">
          <code className="eventsources-token mono">{url}</code>
          <Button variant="outline" onClick={() => copy(url, setCopiedUrl)}>
            {copiedUrl ? t('common:copy.copied') : t('eventSources.token.copy')}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
