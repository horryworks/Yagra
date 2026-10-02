// SPDX-License-Identifier: AGPL-3.0-only
import { useMemo, useState } from 'react';
import { useCopy } from '../lib/useCopy';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { EventSource } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useClientFilters } from '../lib/useClientFilters';
import { eventSourceFilters } from './eventConfigFilters';
import { EditIcon, TrashIcon, PowerIcon, KeyIcon } from '../components/ui/icons';
import './EventSourcesPage.css';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { rowActionsWidth } from '../lib/rowActions';

export function EventSourcesPage() {
  const { t } = useTranslation('alertsConfig');
  const canConfig = useCan('manage_config');
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<EventSource | null>(null);
  const [deleting, setDeleting] = useState<EventSource | null>(null);
  // The one-time token disclosure after create / rotate.
  const [issued, setIssued] = useState<{ id: string; token: string } | null>(null);
  const [error, setError] = useState<string | null>(null);

  const sources = useLoad(() => api.listEventSources(), [], { initial: [] as EventSource[] });
  const { data: rows, loading, reload: load } = sources;


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
        // Rotate, on/off, edit, delete.
        width: rowActionsWidth(4),
        align: 'right',
        render: (r) =>
          canConfig ? (
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
  }, [t, canConfig, rows]);

  const filtering = useClientFilters(columns, rows);
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;

  return (
    <div>
      <PageHeader title={t('nav:events.webhooks')} note={t('eventSources.note')} />
      <LoadGate
        load={sources}
        permission="manage_config"
        unavailable={t('eventSources.unavailable')}>
        <ListToolbar
          list={filtering}
          labels={columnLabels(columns)}
          count={{
            shown: shown.length,
            total: rows.length,
            noun: (n) => t('noun.source', { count: n }),
          }}
        >
          {canConfig && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              {t('eventSources.add')}
            </Button>
          )}
        </ListToolbar>
        {error && <p className="form-error">{error}</p>}
        <DataTable
          tableId="events.sources"
          rows={shown}
          columns={columns}
          rowKey={(r) => r.id}
          filters={filters}
          onFiltersChange={setFilters}
          filterCounts={counts}
          loading={loading}
          empty={anyFiltered ? t('eventSources.emptyMatch') : t('eventSources.empty')}
        />
      </LoadGate>
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
  const form = useSubmit({ errorFallback: t('eventSources.err.add'), onDone });
  const valid = name.trim() !== '';
  const submit = () => {
    if (!valid) return;
    form.submit(() =>
      api
        .createEventSource({ name: name.trim() })
        .then((created) => done(created)),
    );
  };
  return (
    <Modal
      title={t('eventSources.addModal.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('eventSources.addModal.create')}
          canSubmit={valid}
        />
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
      <FormError form={form} />
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
  const form = useSubmit({ errorFallback: t('eventSources.err.save'), onDone });
  const valid = name.trim() !== '';
  const submit = () => {
    if (!valid) return;
    form.submit(() =>
      api
        .updateEventSource(source.id, { name: name.trim(), enabled: source.enabled, node_id: source.node_id })
        .then(() => done()),
    );
  };
  return (
    <Modal
      title={t('eventSources.editModal.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('common:actions.save')}
          canSubmit={valid}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('eventSources.editModal.name')}</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <FormError form={form} />
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
  const { copied, copy } = useCopy();
  const url = `${window.location.origin}/api/v1/ingest/webhook/${issued.id}`;

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
          <Button variant="outline" onClick={() => copy(issued.token, 'token')}>
            {copied === 'token' ? t('common:copy.copied') : t('eventSources.token.copy')}
          </Button>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('eventSources.token.url')}</label>
        <div className="eventsources-copyrow">
          <code className="eventsources-token mono">{url}</code>
          <Button variant="outline" onClick={() => copy(url, 'url')}>
            {copied === 'url' ? t('common:copy.copied') : t('eventSources.token.copy')}
          </Button>
        </div>
      </div>
    </Modal>
  );
}
