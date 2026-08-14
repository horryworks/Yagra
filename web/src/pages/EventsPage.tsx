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
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { eventColumns, eventCard } from '../components/EventLog/eventColumns';
import {
  eventEmptyKind,
  eventFilterColumns,
  eventFilterQuery,
  eventHighlight,
} from '../components/EventLog/eventFilterSpec';
import {
  eventColumnLabels,
  useEventFacets,
  useWidenedEventLog,
  useSearchSemantics,
} from '../components/EventLog/useEventFilters';
import { useFilterParams } from '../lib/useFilterParams';
import { defaultFilters, isAnyFiltered } from '../lib/columnFilter';
import { ClearFilters } from '../components/ui/ClearFilters';
import { readIdParam, writeIdParam } from '../lib/filterParams';

export function EventsPage() {
  const { t } = useTranslation('alerts');
  const [searchParams, setSearchParams] = useSearchParams();
  const nodeId = readIdParam(searchParams, 'node_id');
  const { nodeName } = useEntityNames();
  const semantics = useSearchSemantics();
  const [sheet, setSheet] = useState(false);

  const filterCols = useMemo(() => eventFilterColumns(t, { semantics }), [t, semantics]);
  const { filters, setFilters, nowMs } = useFilterParams(filterCols);

  const query = useMemo(() => eventFilterQuery(filters, nowMs), [filters, nowMs]);
  const facets = useEventFacets(filterCols, filters, nowMs, {
    node_id: nodeId ?? undefined,
  });

  const { rows, loading, exhausted, loadMore, widened } = useWidenedEventLog(query, semantics, {
    node_id: nodeId ?? undefined,
  });

  // Built once per query, not per row: `matchRanges` compiles a pattern, and there are 100 rows on
  // screen. `widened` belongs in here because after the automatic retry the term really was matched
  // inside words, and the marks have to say the same thing the query asked.
  const highlight = useMemo(
    () => eventHighlight(filters, semantics, widened),
    [filters, semantics, widened],
  );
  const columns = useMemo(
    () => eventColumns(nodeName, t, { semantics, highlight }),
    [nodeName, t, semantics, highlight],
  );
  const renderCard = useMemo(() => eventCard(nodeName, t, { highlight }), [nodeName, t, highlight]);

  const anyFiltered = isAnyFiltered(filterCols, filters) || nodeId != null;
  const empty = {
    unfiltered: t('events.emptyWindow'),
    filtered: t('events.empty'),
    prefixMiss: t('events.emptyPrefixMiss'),
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
        <FilterButton
          columns={filterCols}
          filters={filters}
          onOpen={() => {
            // The sheet shows every column at once, so its counts are fetched together rather than
            // per popover — there is no "opened this one" signal on mobile.
            for (const c of filterCols) facets.load(c.key);
            setSheet(true);
          }}
        />
        {/* The node picker is counted and cleared with the columns: it is not a column filter, but
            it narrows this list, and "clear all filters" that leaves a node selected is a lie.
            ⚠️ Both go into ONE `setSearchParams` — see `setFilters`'s `also` parameter for what
            happened when they were two. */}
        <ClearFilters
          columns={filterCols}
          filters={filters}
          extraActive={nodeId != null}
          onClear={() =>
            setFilters(defaultFilters(filterCols), (p) => writeIdParam(p, 'node_id', null))
          }
        />
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={exhausted ? t('events.events') : t('events.eventsLoaded')}
        />
      </TableToolbar>
      {/* Said out loud, because the rows below are the answer to a slightly broader question than
          the one the operator typed. Silently widening would be the worse half of this trade. */}
      {widened && <p className="ev-widened">{t('events.widened')}</p>}
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
