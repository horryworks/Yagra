// SPDX-License-Identifier: AGPL-3.0-only
// Metric sets (Nodes ▸ Metric sets). Reusable, named metric bundles that device profiles attach
// (the design's middle layer: MIB → Metric sets → profile). Edit a set's metrics once and every
// profile that references it updates. CRUD against /collection-templates (the API/type names keep
// the "template" wording; the UI label is "Metric set"); ManageConfig-gated, 503 in skeleton mode
// surfaced.
//
// Data-table standard v2: a toolbar (count + "+ Add metric set") over the shared `.ytable`; add via
// modal, delete via confirm modal. Each row expands to its metric editor.

import { useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api } from '../services/api';
import { useCan } from '../store';
import type { CollectionTemplate } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, RequiredMark } from '../components/ui/Field';
import { IconButton } from '../components/ui/IconButton';
import { ListToolbar } from '../components/ui/ListToolbar';
import { serverToolbarFilters } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useFilterParams } from '../lib/useFilterParams';
import { buildPredicate } from '../lib/filterPredicate';
import { setColumns, setFilterLabels, metricSetFilters } from './monitoringConfigFilters';
import { TrashIcon } from '../components/ui/icons';
import { CollectionEditor } from '../components/CollectionEditor/CollectionEditor';
import './CollectionTemplatesPage.css';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';


export function CollectionTemplatesPage() {
  const { t } = useTranslation('monitoring');
  const canConfig = useCan('manage_config');
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<CollectionTemplate | null>(null);
  const [openItems, setOpenItems] = useState<string | null>(null);

  const sets = useLoad(() => api.listCollectionTemplates(), [], {
    initial: [] as CollectionTemplate[],
  });
  const { data: rows, loading, reload: load } = sets;

  const filterCols = useMemo(() => setColumns(t), [t]);
  // In the URL (ADR-153), so a narrowed list survives a reload.
  const { filters, setFilters } = useFilterParams(filterCols);
  const filtered = useMemo(
    () => rows.filter(buildPredicate(filterCols, filters, Date.now())),
    [rows, filterCols, filters],
  );

  const columns: Column<CollectionTemplate>[] = useMemo(() => {
    const specs = metricSetFilters(t);
    const cols: Column<CollectionTemplate>[] = [
      {
        key: 'name',
        header: t('sets.cols.name'),
        width: '1.6fr',
        render: (r) => <span className="yt-name-txt">{r.name}</span>,
      },
      {
        key: 'description',
        header: t('sets.cols.description'),
        width: '1.6fr',
        render: (r) => <span className="muted ellipsis">{r.description ?? '—'}</span>,
      },
      {
        key: 'metrics',
        header: t('sets.cols.metrics'),
        width: '130px',
        render: (r) => {
          const open = openItems === r.id;
          return (
            <button
              type="button"
              className={`tmpl-metrics-toggle${open ? ' open' : ''}`}
              aria-expanded={open}
              onClick={() => setOpenItems((cur) => (cur === r.id ? null : r.id))}
            >
              {t('shared.metricsCount', { count: r.item_count })}
              <svg className="tmpl-metrics-chev" viewBox="0 0 12 12" aria-hidden="true">
                <path
                  d="M4 2l4 4-4 4"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.4"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                />
              </svg>
            </button>
          );
        },
      },
      {
        key: 'actions',
        header: t('shared.colActions'),
        width: '72px',
        align: 'right',
        render: (r) =>
          canConfig ? (
            <IconButton title={t('sets.deleteSet')} danger onClick={() => setDeleting(r)}>
              <TrashIcon />
            </IconButton>
          ) : null,
      },
    ];
    // ⚠️ Untyped index lookup — rename a column key and its filter cell silently disappears.
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [t, canConfig, openItems]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.metricSets')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.metricSets') }]}
        note={t('sets.note')}
      />

      <LoadGate load={sets} permission="manage_config">
        <ListToolbar
          list={serverToolbarFilters(filterCols, { filters, setFilters })}
          labels={setFilterLabels(t)}
          count={{
            shown: filtered.length,
            total: rows.length,
            noun: (n) => t('sets.noun', { count: n }),
          }}
        >
          {canConfig && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              + {t('sets.addSet')}
            </Button>
          )}
        </ListToolbar>

        {/* Virtualized since ADR-053 Inc.6 — the last of the twelve hand-rolled `ytable` screens
            to move, held back only because its rows expand. `expandedKey` is what drops the
            measured height of a row that has just closed. */}
        <div className="templates-table">
          <DataTable
            tableId="nodes.collectionTemplates"
            rows={filtered}
            columns={columns}
            rowKey={(r) => r.id}
            loading={loading}
            expanded={(r) =>
              openItems === r.id ? (
                <div className="crud-collection">
                  <CollectionEditor scope="template" scopeId={r.id} canEdit={canConfig} />
                </div>
              ) : null
            }
            expandedKey={openItems}
            filters={filters}
            onFiltersChange={setFilters}
            empty={
              <>
                <p className="yt-empty-title">
                  {rows.length === 0 ? t('sets.empty.none') : t('sets.empty.noMatch')}
                </p>
                <p className="yt-empty-sub">
                  {rows.length === 0 ? t('sets.empty.noneSub') : t('shared.trySearch')}
                </p>
              </>
            }
          />
        </div>
      </LoadGate>

      {adding && (
        <AddTemplateModal
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {deleting && (
        <DeleteTemplateModal
          template={deleting}
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

/** Create a collection template (focused-editing modal — name + optional description). */
function AddTemplateModal({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const { t } = useTranslation('monitoring');
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const form = useSubmit({ errorFallback: t('sets.err.create'), onDone });

  const submit = () => {
    if (!name.trim()) return;
    form.submit(() =>
      api
        .createCollectionTemplate({ name: name.trim(), description: description.trim() || undefined })
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('sets.addSet')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('sets.modal.addSubmit')}
          canSubmit={!!name.trim()}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          {t('sets.cols.name')} <RequiredMark />
        </label>
        <TextInput
          placeholder={t('sets.modal.namePlaceholder')}
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('sets.cols.description')}</label>
        <TextInput
          placeholder={t('sets.modal.descPlaceholder')}
          value={description}
          onChange={(e) => setDescription(e.target.value)}
        />
      </div>
      <FormError form={form} />
    </Modal>
  );
}

/** Confirm + delete a template (destructive-consent modal). */
function DeleteTemplateModal({
  template,
  onClose,
  onDone,
}: {
  template: CollectionTemplate;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  return (
    <ConfirmDeleteModal
      title={t('sets.deleteSet')}
      onConfirm={() => api.deleteCollectionTemplate(template.id)}
      errorFallback={t('sets.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="sets.delete.confirm"
        values={{ name: template.name }}
        components={{ strong: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}
