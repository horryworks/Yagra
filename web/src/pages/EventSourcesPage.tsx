// SPDX-License-Identifier: AGPL-3.0-only
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { EventSource } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { eventSourceFilters } from './eventConfigFilters';
import { EditIcon, TrashIcon, PowerIcon, KeyIcon } from '../components/ui/icons';
import './EventSourcesPage.css';

export function EventSourcesPage() {
  const { t } = useTranslation('alertsConfig');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<EventSource[]>([]);
  const [sheet, setSheet] = useState(false);
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

  const columns = useMemo<Column<EventSource>[]>(() => {
    // The kind list comes from the rows, so a source kind a newer core introduced is selectable
    // rather than silently missing from the filter.
    const kinds = [...new Set(rows.map((r) => r.kind))].sort();
    const specs = eventSourceFilters(t, kinds);
    const cols: Column<EventSource>[] = [
      { key: 'name', header: t('eventSources.cols.name'), width: '1.6fr', render: (r) => r.name },
      {
        key: 'kind',
        header: t('eventSources.cols.kind'),
        width: '120px',
        render: (r) => <Badge tone="neutral">{r.kind}</Badge>,
      },
      {
        key: 'status',
        header: t('eventSources.cols.status'),
        width: '110px',
        render: (r) => (
          <Badge tone={r.enabled ? 'up' : 'neutral'}>
            {r.enabled ? t('status.enabled') : t('status.disabled')}
          </Badge>
        ),
      },
      {
        key: 'actions',
        header: t('eventSources.cols.actions'),
        width: '130px',
        align: 'right',
        render: (r) =>
          authed ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  { label: t('eventSources.rotate'), icon: <KeyIcon />, onClick: () => rotate(r) },
                  {
                    label: r.enabled ? t('eventSources.disable') : t('eventSources.enable'),
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
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
    // `rotate` and `toggleEnabled` are rebuilt every render; listing them would rebuild the
    // columns on every keystroke elsewhere and re-run the predicate for nothing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, authed, rows]);

  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { url: true },
  );

  return (
    <div>
      <PageHeader title={t('nav:events.webhooks')} note={t('eventSources.note')} />
      {unavailable ? (
        <Card>{t('eventSources.unavailable')}</Card>
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('noun.source', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('eventSources.add')}
              </Button>
            )}
          </TableToolbar>
          {error && <p className="form-error">{error}</p>}
          <DataTable
            rows={shown}
            columns={columns}
            rowKey={(r) => r.id}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            loading={loading}
            empty={anyFiltered ? t('eventSources.emptyMatch') : t('eventSources.empty')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
              onClose={() => setSheet(false)}
            />
          )}
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
  return (
    <ConfirmDeleteModal
      title={t('eventSources.deleteModal.title')}
      onConfirm={() => api.deleteEventSource(source.id)}
      errorFallback={t('eventSources.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="eventSources.deleteModal.body"
        values={{ name: source.name }}
        components={{ strong: <strong /> }}
      />
    </ConfirmDeleteModal>
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
