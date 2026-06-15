// MIB repository (Nodes ▸ MIB repository). A curated, searchable OID catalog: metric_name →
// (OID, kind, vendor). Seeded from the built-in standard + vendor OID sets; admins can add
// their own. The collection editor picks from this so operators choose metrics by name instead
// of typing raw OIDs.
//
// Data-table standard v2: a toolbar (debounced server search + count + "+ Add entry") over the
// shared `.ytable`. Add and delete go through modals (focused-editing / destructive-consent).

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionKind, MetricKind, MibCatalogEntry } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import './MibRepositoryPage.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const OID_RE = /^[0-9]+(\.[0-9]+)*$/;

const COLS = '1.4fr 2fr 1fr 1fr 92px';

/** Create a catalog entry (focused-editing modal). Same fields + OID gate as the old inline row. */
function AddMibEntryModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [metricName, setMetricName] = useState('');
  const [oid, setOid] = useState('');
  const [collection, setCollection] = useState<CollectionKind>('scalar');
  const [metricKind, setMetricKind] = useState<MetricKind>('gauge');
  const [vendor, setVendor] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const valid = metricName.trim().length > 0 && OID_RE.test(oid.trim());

  const submit = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    api
      .createMibEntry({
        metric_name: metricName.trim(),
        oid: oid.trim(),
        collection,
        metric_kind: metricKind,
        vendor: vendor.trim() || undefined,
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add entry'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add catalog entry"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            Add entry
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Metric name</label>
        <TextInput
          placeholder="metric_name"
          value={metricName}
          onChange={(e) => setMetricName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">OID</label>
        <TextInput
          className="mono"
          placeholder="OID (e.g. 1.3.6.1.2.1.1.3.0)"
          value={oid}
          onChange={(e) => setOid(e.target.value)}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Collection</label>
        <Select
          value={collection}
          onChange={(e) => setCollection(e.target.value as CollectionKind)}
        >
          <option value="scalar">scalar</option>
          <option value="table">table</option>
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Metric kind</label>
        <Select value={metricKind} onChange={(e) => setMetricKind(e.target.value as MetricKind)}>
          <option value="gauge">gauge</option>
          <option value="counter">counter</option>
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Vendor</label>
        <TextInput
          placeholder="vendor (optional)"
          value={vendor}
          onChange={(e) => setVendor(e.target.value)}
        />
        <span className="modal-hint">Leave blank for a standard (non-vendor) OID.</span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a catalog entry (destructive-consent modal). */
function DeleteMibEntryModal({
  entry,
  onClose,
  onDone,
}: {
  entry: MibCatalogEntry;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteMibEntry(entry.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete entry'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete catalog entry"
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
        Delete catalog entry <strong>{entry.metric_name}</strong>? Collections referencing it by
        name will no longer resolve from the catalog. This cannot be undone.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function MibRepositoryPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<MibCatalogEntry[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<MibCatalogEntry | null>(null);

  const load = useCallback((q: string) => {
    api
      .listMibCatalog(q.trim() || undefined)
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
    const t = setTimeout(() => load(query), 200);
    return () => clearTimeout(t);
  }, [load, query]);

  return (
    <div>
      <PageHeader
        title="MIB repository"
        trail={[{ label: 'Nodes' }, { label: 'MIB repository' }]}
        note="A curated OID catalog. Pick metrics by name in the collection editor instead of typing OIDs."
      />

      {unavailable ? (
        <Card>
          <p className="muted">The MIB catalog is unavailable in skeleton mode.</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search metric / OID / vendor…"
              ariaLabel="Search MIB catalog"
            />
            <TableSpacer />
            <ResultCount shown={rows.length} noun="entries" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add entry
              </Button>
            )}
          </TableToolbar>

          <div className="ytable">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h">Metric</div>
                <div className="ytable-h">OID</div>
                <div className="ytable-h">Type</div>
                <div className="ytable-h">Vendor</div>
                <div className="ytable-h right">Actions</div>
              </div>

              {rows.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">
                    {loading ? 'Loading…' : 'No catalog entries match'}
                  </p>
                  {!loading && <p className="yt-empty-sub">Try a different search.</p>}
                </div>
              ) : (
                rows.map((e) => (
                  <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={e.id}>
                    <div className="ytable-cell mib-metric">{e.metric_name}</div>
                    <div className="ytable-cell mono ellipsis">{e.oid}</div>
                    <div className="ytable-cell">
                      {e.collection} · {e.metric_kind}
                    </div>
                    <div className="ytable-cell">
                      {e.vendor ? (
                        <Badge tone="neutral">{e.vendor}</Badge>
                      ) : (
                        <span className="muted">standard</span>
                      )}
                    </div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton title="Delete" danger onClick={() => setDeleting(e)}>
                            <TrashIcon />
                          </IconButton>
                        </span>
                      )}
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
        </>
      )}

      {adding && (
        <AddMibEntryModal onClose={() => setAdding(false)} onSaved={() => load(query)} />
      )}
      {deleting && (
        <DeleteMibEntryModal
          entry={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load(query);
          }}
        />
      )}
    </div>
  );
}
