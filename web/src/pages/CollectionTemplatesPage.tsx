// Metric sets (Nodes ▸ Metric sets). Reusable, named metric bundles that device profiles attach
// (the design's middle layer: MIB → Metric sets → profile). Edit a set's metrics once and every
// profile that references it updates. CRUD against /collection-templates (the API/type names keep
// the "template" wording; the UI label is "Metric set"); ManageConfig-gated, 503 in skeleton mode
// surfaced.
//
// Data-table standard v2: a toolbar (count + "+ Add metric set") over the shared `.ytable`; add via
// modal, delete via confirm modal. Each row expands to its metric editor.

import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionTemplate } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, RequiredMark } from '../components/ui/Field';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import { CollectionEditor } from '../components/CollectionEditor/CollectionEditor';
import './CollectionTemplatesPage.css';

const COLS = '1.6fr 1.6fr 130px 72px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function CollectionTemplatesPage() {
  const { t } = useTranslation('monitoring');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<CollectionTemplate[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<CollectionTemplate | null>(null);
  const [openItems, setOpenItems] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .listCollectionTemplates()
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
    if (q === '') return rows;
    return rows.filter(
      (tmpl) =>
        tmpl.name.toLowerCase().includes(q) || (tmpl.description ?? '').toLowerCase().includes(q),
    );
  }, [rows, query]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.metricSets')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.metricSets') }]}
        note={t('sets.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('sets.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('sets.searchPlaceholder')}
              ariaLabel={t('sets.searchAria')}
            />
            <TableSpacer />
            <ResultCount
              shown={filtered.length}
              total={rows.length}
              noun={t('sets.noun', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('sets.addSet')}
              </Button>
            )}
          </TableToolbar>

          <div className="ytable templates-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('sets.cols.name')}</div>
              <div className="ytable-h">{t('sets.cols.description')}</div>
              <div className="ytable-h">{t('sets.cols.metrics')}</div>
              <div className="ytable-h right">{t('shared.colActions')}</div>
            </div>

            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? t('common:loading')
                    : rows.length === 0
                      ? t('sets.empty.none')
                      : t('sets.empty.noMatch')}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0 ? t('sets.empty.noneSub') : t('shared.trySearch')}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((tmpl) => {
                const open = openItems === tmpl.id;
                return (
                  <Fragment key={tmpl.id}>
                    <div className="ytable-row" style={{ gridTemplateColumns: COLS }}>
                      <div className="ytable-cell">
                        <span className="yt-name-txt">{tmpl.name}</span>
                      </div>
                      <div className="ytable-cell ellipsis">
                        <span className="muted">{tmpl.description ?? '—'}</span>
                      </div>
                      <div className="ytable-cell">
                        <button
                          type="button"
                          className={`tmpl-metrics-toggle${open ? ' open' : ''}`}
                          aria-expanded={open}
                          onClick={() => setOpenItems((cur) => (cur === tmpl.id ? null : tmpl.id))}
                        >
                          {t('shared.metricsCount', { count: tmpl.item_count })}
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
                      </div>
                      <div className="ytable-cell right">
                        {authed && (
                          <span className="ytable-actions">
                            <IconButton title={t('sets.deleteSet')} danger onClick={() => setDeleting(tmpl)}>
                              <TrashIcon />
                            </IconButton>
                          </span>
                        )}
                      </div>
                    </div>
                    {open && (
                      <div className="crud-collection">
                        <CollectionEditor scope="template" scopeId={tmpl.id} canEdit={authed} />
                      </div>
                    )}
                  </Fragment>
                );
              })
            )}
          </div>
        </>
      )}

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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    if (!name.trim()) return;
    setBusy(true);
    setError(null);
    api
      .createCollectionTemplate({ name: name.trim(), description: description.trim() || undefined })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('sets.err.create')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('sets.addSet')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!name.trim() || busy}>
            {t('sets.modal.addSubmit')}
          </Button>
        </>
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
      {error && <p className="form-error">{error}</p>}
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteCollectionTemplate(template.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('sets.err.delete')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('sets.deleteSet')}
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
          i18nKey="sets.delete.confirm"
          values={{ name: template.name }}
          components={{ strong: <strong /> }}
        />
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
