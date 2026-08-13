// SPDX-License-Identifier: AGPL-3.0-only
// Events (Alerts ▸ Events). Append-only log of received passive events (syslog / SNMP traps /
// webhooks), keyset-paged newest-first. The rule-authoring surface: browse what devices actually
// send, then write rules against it.
//
// **Every filter lives in the URL** (ADR-053). It used to be one param (`node_id`) plus five pieces
// of component state, so a link to "trap events mentioning BGP in the last week" could not be sent
// to anyone. The column keys are the URL keys — see `lib/columnFilter.ts` for why there is no
// prefix — and a filter at its default deletes its key, so a bare `/alerts/events` is always the
// default view. Fetch/paging, columns and the filter descriptors are shared with the NodeDetail
// Events tab via components/EventLog. Empty in skeleton mode.

import { useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { useEntityNames } from '../components/ui/EntityName';
import { DataTable } from '../components/ui/DataTable';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { useEventLog } from '../components/EventLog/useEventLog';
import { eventColumns, eventCard } from '../components/EventLog/eventColumns';
import {
  eventEmptyKind,
  eventFilterColumns,
  eventFilterQuery,
} from '../components/EventLog/eventFilterSpec';
import {
  eventColumnLabels,
  useEventFacets,
  useSearchSemantics,
} from '../components/EventLog/useEventFilters';
import { useFilterParams } from '../lib/useFilterParams';
import { isAnyFiltered } from '../lib/columnFilter';
import { readIdParam, writeIdParam } from '../lib/filterParams';

export function EventsPage() {
  const { t } = useTranslation('alerts');
  const [searchParams, setSearchParams] = useSearchParams();
  const nodeId = readIdParam(searchParams, 'node_id');
  const { nodeName } = useEntityNames();
  const semantics = useSearchSemantics();
  const [sheet, setSheet] = useState(false);

  const columns = useMemo(
    () => eventColumns(nodeName, t, { semantics }),
    [nodeName, t, semantics],
  );
  const renderCard = useMemo(() => eventCard(nodeName, t), [nodeName, t]);
  const filterCols = useMemo(() => eventFilterColumns(t, { semantics }), [t, semantics]);
  const { filters, setFilters, nowMs } = useFilterParams(filterCols);

  const query = useMemo(() => eventFilterQuery(filters, nowMs), [filters, nowMs]);
  const facets = useEventFacets(filterCols, filters, nowMs, {
    node_id: nodeId ?? undefined,
  });

  const { rows, loading, exhausted, loadMore } = useEventLog({
    ...query,
    node_id: nodeId ?? undefined,
  });

  const anyFiltered = isAnyFiltered(filterCols, filters) || nodeId != null;
  const empty = {
    unfiltered: t('events.emptyWindow'),
    filtered: t('events.empty'),
    tokenMiss: t('events.emptyTokenMiss'),
  }[eventEmptyKind(filters, semantics, anyFiltered)];

  const setNode = (node: { id: string; name: string } | null) => {
    const params = new URLSearchParams(searchParams);
    writeIdParam(params, 'node_id', node?.id ?? null);
    setSearchParams(params, { replace: true });
  };

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:alerts.events')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.events') }]}
        note={t('events.note')}
      />
      {/* An action row, not a filter bar. The node picker stays here rather than becoming the
          Source column's filter: it resolves a name to an id against the inventory, which is a
          different question from "does this row's source contain these characters", and nesting its
          own popover inside a filter popover would clip it (ui-conventions). */}
      <TableToolbar>
        <NodePicker
          value={nodeId ?? null}
          valueLabel={nodeId ? nodeName(nodeId) : undefined}
          onChange={setNode}
          placeholder={t('nav:nodes.all')}
        />
        <MobileFilterButton
          columns={filterCols}
          filters={filters}
          onOpen={() => {
            // The sheet shows every column at once, so its counts are fetched together rather than
            // per popover — there is no "opened this one" signal on mobile.
            for (const c of filterCols) facets.load(c.key);
            setSheet(true);
          }}
        />
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={exhausted ? t('events.events') : t('events.eventsLoaded')}
        />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={facets.counts}
        onFilterOpen={facets.load}
        renderCard={renderCard}
        cardEstimatePx={92}
        rowKey={(r) => r.id}
        onReachEnd={loadMore}
        empty={empty}
        loading={loading}
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          counts={facets.counts}
          labels={eventColumnLabels(t)}
          onClose={() => setSheet(false)}
        />
      )}
    </div>
  );
}
