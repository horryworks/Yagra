// Collection templates (Nodes ▸ Collection templates). Reusable, named metric bundles that
// device profiles attach (the design's middle layer: MIB → Collection templates → profile).
// Edit a template's metrics once and every profile that references it updates. CRUD against
// /collection-templates; ManageConfig-gated, 503 in skeleton mode surfaced.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionTemplate } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import { CollectionEditor } from '../components/CollectionEditor/CollectionEditor';
import './CrudList.css';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

export function CollectionTemplatesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<CollectionTemplate[]>([]);
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [unavailable, setUnavailable] = useState(false);
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
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const add = () => {
    setError(null);
    api
      .createCollectionTemplate({
        name: name.trim(),
        description: description.trim() || undefined,
      })
      .then(() => {
        setName('');
        setDescription('');
        load();
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to create template')));
  };

  const remove = (id: string) =>
    api
      .deleteCollectionTemplate(id)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to delete template')));

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
        <Card title="Templates">
          {authed && (
            <div className="crud-add form-row">
              <TextInput
                placeholder="Template name (e.g. Standard interfaces)"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
              <TextInput
                placeholder="Description (optional)"
                value={description}
                onChange={(e) => setDescription(e.target.value)}
              />
              <Button variant="primary" onClick={add} disabled={!name.trim()}>
                Add template
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}
          {rows.length === 0 ? (
            <p className="muted">No collection templates yet.</p>
          ) : (
            <div className="crud-list">
              {rows.map((t) => (
                <div key={t.id}>
                  <div className="crud-row">
                    <span className="crud-name">{t.name}</span>
                    <span className="crud-id">
                      {t.description ?? ''}
                      <span className="muted"> · {t.item_count} metrics</span>
                    </span>
                    <Button
                      variant="ghost"
                      onClick={() => setOpenItems((cur) => (cur === t.id ? null : t.id))}
                    >
                      {openItems === t.id ? 'Hide metrics' : 'Metrics'}
                    </Button>
                    {authed && (
                      <Button variant="ghost" onClick={() => remove(t.id)}>
                        Delete
                      </Button>
                    )}
                  </div>
                  {openItems === t.id && (
                    <div className="crud-collection">
                      <CollectionEditor scope="template" scopeId={t.id} canEdit={authed} />
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
