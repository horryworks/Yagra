// SPDX-License-Identifier: AGPL-3.0-only
// MIB repository (Nodes ▸ MIB repository). A curated, searchable OID catalog: metric_name →
// (OID, kind, vendor). Seeded from the built-in standard + vendor OID sets; admins can add
// their own. The collection editor picks from this so operators choose metrics by name instead
// of typing raw OIDs.
//
// Data-table standard v2: a toolbar (debounced server search + count + "+ Add entry") over the
// shared `.ytable`. Add and delete go through modals (focused-editing / destructive-consent).

import { useCallback, useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionKind, MetricKind, MibCatalogEntry } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import './MibRepositoryPage.css';

const OID_RE = /^[0-9]+(\.[0-9]+)*$/;

const COLS = '1.4fr 2fr 1fr 1fr 92px';

/** Create a catalog entry (focused-editing modal). Same fields + OID gate as the old inline row. */
function AddMibEntryModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('monitoring');
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
        setError(errMsg(e, t('mib.err.add')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('mib.addTitle')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {t('mib.addEntry')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('mib.modal.metricName')}</label>
        <TextInput
          placeholder={t('mib.modal.metricNamePlaceholder')}
          value={metricName}
          onChange={(e) => setMetricName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('mib.modal.oid')}</label>
        <TextInput
          className="mono"
          placeholder={t('mib.modal.oidPlaceholder')}
          value={oid}
          onChange={(e) => setOid(e.target.value)}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('mib.modal.collection')}</label>
        <Select
          value={collection}
          onChange={(e) => setCollection(e.target.value as CollectionKind)}
        >
          <option value="scalar">{t('enum.scalar')}</option>
          <option value="table">{t('enum.table')}</option>
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('mib.modal.metricKind')}</label>
        <Select value={metricKind} onChange={(e) => setMetricKind(e.target.value as MetricKind)}>
          <option value="gauge">{t('enum.gauge')}</option>
          <option value="counter">{t('enum.counter')}</option>
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('mib.modal.vendor')}</label>
        <TextInput
          placeholder={t('mib.modal.vendorPlaceholder')}
          value={vendor}
          onChange={(e) => setVendor(e.target.value)}
        />
        <span className="modal-hint">{t('mib.modal.vendorHint')}</span>
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
  const { t } = useTranslation('monitoring');
  return (
    <ConfirmDeleteModal
      title={t('mib.deleteTitle')}
      onConfirm={() => api.deleteMibEntry(entry.id)}
      errorFallback={t('mib.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="mib.delete.confirm"
        values={{ name: entry.metric_name }}
        components={{ strong: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}

export function MibRepositoryPage() {
  const { t } = useTranslation('monitoring');
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
    const timer = setTimeout(() => load(query), 200);
    return () => clearTimeout(timer);
  }, [load, query]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.mib')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.mib') }]}
        note={t('mib.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('mib.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('mib.searchPlaceholder')}
              ariaLabel={t('mib.searchAria')}
            />
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('mib.noun', { count: rows.length })} />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('mib.addEntry')}
              </Button>
            )}
          </TableToolbar>

          <div className="ytable">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h">{t('mib.cols.metric')}</div>
                <div className="ytable-h">{t('mib.cols.oid')}</div>
                <div className="ytable-h">{t('mib.cols.type')}</div>
                <div className="ytable-h">{t('mib.cols.vendor')}</div>
                <div className="ytable-h right">{t('shared.colActions')}</div>
              </div>

              {rows.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">
                    {loading ? t('common:loading') : t('mib.empty.noMatch')}
                  </p>
                  {!loading && <p className="yt-empty-sub">{t('shared.trySearch')}</p>}
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
                        <span className="muted">{t('mib.standard')}</span>
                      )}
                    </div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton title={t('common:actions.delete')} danger onClick={() => setDeleting(e)}>
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
