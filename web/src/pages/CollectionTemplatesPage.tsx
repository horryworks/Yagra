// Collection templates (Nodes ▸ Collection templates). Reusable, named metric bundles that
// device profiles attach (the design's middle layer: MIB → Collection templates → profile).
// Edit a template's metrics once and every profile that references it updates. CRUD against
// /collection-templates; ManageConfig-gated, 503 in skeleton mode surfaced.
//
// Data-table standard v2: a toolbar (count + "+ Add template") over the shared `.ytable`; add via
// modal, delete via confirm modal. Each row expands to its Collection-set editor.

import { Fragment, useCallback, useEffect, useMemo, useState } from 'react';
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
      (t) =>
        t.name.toLowerCase().includes(q) || (t.description ?? '').toLowerCase().includes(q),
    );
  }, [rows, query]);

  return (
    <div>
      <PageHeader
        title="Collection templates"
        trail={[{ label: 'Nodes' }, { label: 'Collection templates' }]}
        note="Reusable metric bundles. Attach them to device profiles; editing one updates every profile that uses it."
      />

      {unavailable ? (
        <Card>
          <p className="muted">
            Collection templates are unavailable in skeleton mode (no metadata store).
          </p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search name or description…"
              ariaLabel="Search collection templates"
            />
            <TableSpacer />
            <ResultCount shown={filtered.length} total={rows.length} noun="templates" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add template
              </Button>
            )}
          </TableToolbar>

          <div className="ytable templates-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Name</div>
              <div className="ytable-h">Description</div>
              <div className="ytable-h">Metrics</div>
              <div className="ytable-h right">Actions</div>
            </div>

            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? 'Loading…'
                    : rows.length === 0
                      ? 'No collection templates yet'
                      : 'No templates match'}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0
                      ? 'Add a reusable metric bundle (e.g. “Standard interfaces”).'
                      : 'Try a different search.'}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((t) => {
                const open = openItems === t.id;
                return (
                  <Fragment key={t.id}>
                    <div className="ytable-row" style={{ gridTemplateColumns: COLS }}>
                      <div className="ytable-cell">
                        <span className="yt-name-txt">{t.name}</span>
                      </div>
                      <div className="ytable-cell ellipsis">
                        <span className="muted">
                          {t.description ?? '—'} · {t.item_count} metrics
                        </span>
                      </div>
                      <div className="ytable-cell">
                        <Button
                          variant="ghost"
                          onClick={() => setOpenItems((cur) => (cur === t.id ? null : t.id))}
                        >
                          {open ? 'Hide metrics' : 'Metrics'}
                        </Button>
                      </div>
                      <div className="ytable-cell right">
                        {authed && (
                          <span className="ytable-actions">
                            <IconButton title="Delete template" danger onClick={() => setDeleting(t)}>
                              <TrashIcon />
                            </IconButton>
                          </span>
                        )}
                      </div>
                    </div>
                    {open && (
                      <div className="crud-collection">
                        <CollectionEditor scope="template" scopeId={t.id} canEdit={authed} />
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
        setError(errMsg(e, 'failed to create template'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add collection template"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!name.trim() || busy}>
            Add template
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          Name <RequiredMark />
        </label>
        <TextInput
          placeholder="e.g. Standard interfaces"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Description</label>
        <TextInput
          placeholder="Optional"
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteCollectionTemplate(template.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete template'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete template"
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
        Delete template <strong>{template.name}</strong>? Profiles that attach it lose this bundle's
        metrics.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
