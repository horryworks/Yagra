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

import { useCallback, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import { useDebouncedValue } from '../lib/useDebouncedValue';
import { api } from '../services/api';
import { useCan } from '../store';
import type { CollectionKind, MetricKind, MibCatalogEntry } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { filterableColumns, type FilterState } from '../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../lib/filterCondition';
import { TrashIcon } from '../components/ui/icons';
import { mibEntryReady } from './mibEntryForm';
import './MibRepositoryPage.css';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';

/** Create a catalog entry (focused-editing modal). Same fields + OID gate as the old inline row. */
function AddMibEntryModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('monitoring');
  const [metricName, setMetricName] = useState('');
  const [oid, setOid] = useState('');
  const [collection, setCollection] = useState<CollectionKind>('scalar');
  const [metricKind, setMetricKind] = useState<MetricKind>('gauge');
  const [vendor, setVendor] = useState('');
  const form = useSubmit({
    errorFallback: t('mib.err.add'),
    onDone: () => {
      onSaved();
      onClose();
    },
  });

  const valid = mibEntryReady(metricName, oid);

  const submit = () => {
    if (!valid) return;
    form.submit(() =>
      api
        .createMibEntry({
          metric_name: metricName.trim(),
          oid: oid.trim(),
          collection,
          metric_kind: metricKind,
          vendor: vendor.trim() || undefined,
        })
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('mib.addTitle')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('mib.addEntry')}
          canSubmit={valid}
        />
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
      <FormError form={form} />
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
  // The search term is `?q=` (ADR-153) — the API's own parameter name — so a reload keeps it. Read
  // and written raw, not trimmed: the filter cell echoes this value back into the box it came from,
  // and a trimmed echo would eat a trailing space while the operator is still typing. `load` trims.
  const [params, setParams] = useSearchParams();
  const query = params.get('q') ?? '';
  const setQuery = useCallback(
    (term: string) => {
      const next = new URLSearchParams(params);
      if (term) next.set('q', term);
      else next.delete('q');
      setParams(next, { replace: true });
    },
    [params, setParams],
  );
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<MibCatalogEntry | null>(null);

  // The term settles, then one load runs for it; an answer to an earlier term that arrives late
  // is dropped (useLoad).
  const settledQuery = useDebouncedValue(query);
  const catalog = useLoad(() => api.listMibCatalog(settledQuery.trim() || undefined), [settledQuery], {
    initial: [] as MibCatalogEntry[],
  });
  const { data: rows, loading, reload: load } = catalog;

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
  const list = {
    filterCols,
    filters,
    setFilters: onFiltersChange,
    clear: () => setQuery(''),
    anyFiltered: !!query,
  };

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.mib')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.mib') }]}
        note={t('mib.note')}
      />

      <LoadGate load={catalog} unavailable={t('mib.unavailable')}>
        <ListToolbar
          list={list}
          labels={columnLabels(columns)}
          count={{ shown: rows.length, noun: (n) => t('mib.noun', { count: n }) }}
        >
          {canConfig && (
            <Button variant="primary" onClick={() => setAdding(true)}>
              + {t('mib.addEntry')}
            </Button>
          )}
        </ListToolbar>

        <DataTable
          tableId="nodes.mib"
          rows={rows}
          columns={columns}
          rowKey={(e) => e.id}
          filters={filters}
          onFiltersChange={onFiltersChange}
          loading={loading}
          empty={t('mib.empty.noMatch')}
        />
      </LoadGate>

      {adding && (
        <AddMibEntryModal onClose={() => setAdding(false)} onSaved={load} />
      )}
      {deleting && (
        <DeleteMibEntryModal
          entry={deleting}
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
