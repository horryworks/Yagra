// SPDX-License-Identifier: AGPL-3.0-only
// MIB repository (Nodes ▸ MIB repository). A curated, searchable OID catalog: metric_name →
// (OID, kind, vendor). Seeded from the built-in standard + vendor OID sets; admins can add
// their own. The collection editor picks from this so operators choose metrics by name instead
// of typing raw OIDs.
//
// Data-table standard v2: an action row (count + "+ Add entry") over the shared `DataTable`, with
// the search in the filter row under the Metric column (ADR-053 Inc.5). Add and delete go through
// modals (focused-editing / destructive-consent).
//
// ⚠️ **Only the Metric column carries a filter, and the other three deliberately do not.** The
// catalog is read with a server-side `LIMIT` (`mib.rs::MibRepo::list`), so the browser holds a
// *prefix* of the matching entries. A client-side predicate over that prefix would narrow what
// happened to arrive and present it as the answer — the failure `ui-conventions.md` calls out for
// scale-aware lists, and the one Settings ▸ Audit shipped with before its filters moved into SQL.
// Type and Vendor become filterable when the endpoint takes them, not before.
//
// The Metric cell's condition is the same server search the toolbar used to hold — it matches the
// metric name, the OID **and** the vendor (`mib-catalog?q=`), so a term typed under "Metric" can
// match on the other two. That imprecision is stated rather than hidden, the same call
// `auditQuery.ts` makes about its own two-column `q`.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { useDebouncedValue } from '../lib/useDebouncedValue';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { CollectionKind, MetricKind, MibCatalogEntry } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { filterableColumns, type FilterState } from '../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../lib/filterCondition';
import { TrashIcon } from '../components/ui/icons';
import { mibEntryReady } from './mibEntryForm';
import './MibRepositoryPage.css';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';

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

  const valid = mibEntryReady(metricName, oid);

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
  const canConfig = useCan('manage_config');
  const [rows, setRows] = useState<MibCatalogEntry[]>([]);
  const [query, setQuery] = useState('');
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<MibCatalogEntry | null>(null);
  const [sheet, setSheet] = useState(false);

  const load = useCallback((q: string) => {
    api
      .listMibCatalog(q.trim() || undefined)
      .then((list) => {
        setRows(list);
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
  }, []);

  // The term settles, then one load runs for it. (No stale-response guard here, deliberately: the
  // catalog is a single admin-scoped list and the last write wins either way.)
  const settledQuery = useDebouncedValue(query);
  useEffect(() => {
    load(settledQuery);
  }, [load, settledQuery]);

  const columns = useMemo<Column<MibCatalogEntry>[]>(
    () => [
      {
        key: 'metric',
        header: t('mib.cols.metric'),
        width: '1.4fr',
        render: (e) => <span className="mib-metric">{e.metric_name}</span>,
        filter: {
          kind: 'text',
          // Contains only: `?q=` is a substring match with no regex parameter and no negated form.
          modes: ['contains'],
          // Server-side — `load()` re-fetches on the settled term, so this is never consulted.
          readText: () => [],
          containsSemantics: 'substring',
          placeholder: t('mib.cols.metric'),
        },
      },
      {
        key: 'oid',
        header: t('mib.cols.oid'),
        width: '2fr',
        render: (e) => <span className="mono ellipsis">{e.oid}</span>,
      },
      {
        key: 'type',
        header: t('mib.cols.type'),
        width: '1fr',
        render: (e) => (
          <>
            {e.collection} · {e.metric_kind}
          </>
        ),
      },
      {
        key: 'vendor',
        header: t('mib.cols.vendor'),
        width: '1fr',
        render: (e) =>
          e.vendor ? (
            <Badge tone="neutral">{e.vendor}</Badge>
          ) : (
            <span className="muted">{t('mib.standard')}</span>
          ),
      },
      {
        key: 'actions',
        header: t('shared.colActions'),
        width: '92px',
        align: 'right',
        render: (e) =>
          canConfig ? (
            <span className="ytable-actions">
              <IconButton
                title={t('common:actions.delete')}
                danger
                onClick={() => setDeleting(e)}
              >
                <TrashIcon />
              </IconButton>
            </span>
          ) : null,
      },
    ],
    [t, canConfig],
  );

  // The filter row is a *view* of `query`, not a second copy of it. One state, one writer — the
  // shape `one-handler-one-url-write` argues for, and the reason there is no `useClientFilters`
  // here: the predicate is the server's.
  const filterCols = useMemo(() => filterableColumns(columns), [columns]);
  const filters: FilterState = useMemo(
    () => ({ metric: query ? encodeCondition({ term: query, mode: 'contains', not: false }) : '' }),
    [query],
  );
  const onFiltersChange = (next: FilterState) => setQuery(decodeCondition(next.metric ?? '').term);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.mib')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.mib') }]}
        note={t('mib.note')}
      />

      {block ? (
        <LoadBlockNotice block={block} unavailable={t('mib.unavailable')} />
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={() => setQuery('')} />
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('mib.noun', { count: rows.length })} />
            {canConfig && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('mib.addEntry')}
              </Button>
            )}
          </TableToolbar>

          <DataTable
            rows={rows}
            columns={columns}
            rowKey={(e) => e.id}
            filters={filters}
            onFiltersChange={onFiltersChange}
            loading={loading}
            empty={t('mib.empty.noMatch')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={onFiltersChange}
              counts={{}}
              labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
              onClose={() => setSheet(false)}
            />
          )}
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
