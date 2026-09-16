// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Events tab: this node's received passive events (syslog / SNMP traps / webhooks) as
// a full keyset-paged log, reusing the shared event-log hook + columns + filter descriptors. The
// Source column and its filter are both dropped — every row is this node — and "Open in Events →"
// deep-links to the node-filtered Events page.
//
// The filters live in the URL under `events.` (ADR-153). They used to be local state, on the
// argument that a node page's URL means "this node" and that more keys would collide with the next
// tab's — and a reload threw them away. The collision was real (the inventory tree on `/nodes` owns
// a bare `kind` too), which is what the prefix answers; the route ledger in
// `filterSpecRegistry.test.ts` checks the keys stay disjoint.

import { useMemo, useState } from 'react';
import { useFilterParams } from '../../lib/useFilterParams';
import { nodeTabFilterPrefix } from './tabs';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { NodeDetail } from '../../types/api';
import { DataTable } from '../ui/DataTable';
import { FilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { TableToolbar, TableSpacer, ResultCount } from '../ui/TableToolbar';
import { useEntityNames } from '../ui/entityNames';
import { eventColumns, eventCard } from '../EventLog/eventColumns';
import {
  eventEmptyKind,
  eventFilterColumns,
  eventFilterQuery,
  eventHighlight,
} from '../EventLog/eventFilterSpec';
import {
  eventColumnLabels,
  useEventFacets,
  useSearchSemantics,
  useWidenedEventLog,
} from '../EventLog/useEventFilters';
import { defaultFilters, isAnyFiltered } from '../../lib/columnFilter';
import { ClearFilters } from '../ui/ClearFilters';

export function EventsTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('alerts');
  const { nodeName } = useEntityNames();
  const semantics = useSearchSemantics();
  const [sheet, setSheet] = useState(false);

  const filterCols = useMemo(
    () => eventFilterColumns(t, { showSource: false, semantics }),
    [t, semantics],
  );
  // `nowMs` is resolved when the range changes, never per request — a lower bound that creeps
  // forward between "load older" pages drops rows the keyset cursor was walking towards
  // (`boundsFor`). `useFilterParams` pins it on exactly that rule; this tab used to hold a second
  // copy of it.
  const { filters, setFilters, nowMs } = useFilterParams(filterCols, nodeTabFilterPrefix('events'));

  const query = useMemo(() => eventFilterQuery(filters, nowMs), [filters, nowMs]);
  const facets = useEventFacets(filterCols, filters, nowMs, { node_id: node.id });

  const { rows, loading, exhausted, loadMore, widened } = useWidenedEventLog(query, semantics, {
    node_id: node.id,
  });

  const highlight = useMemo(
    () => eventHighlight(filters, semantics, widened),
    [filters, semantics, widened],
  );
  const columns = useMemo(
    () => eventColumns(nodeName, t, { showSource: false, semantics, highlight }),
    [nodeName, t, semantics, highlight],
  );
  const renderCard = useMemo(
    () => eventCard(nodeName, t, { showSource: false, highlight }),
    [nodeName, t, highlight],
  );

  const empty = {
    unfiltered: t('eventLog.emptyNodeWindow'),
    filtered: t('eventLog.emptyNode'),
    prefixMiss: t('events.emptyPrefixMiss'),
  }[eventEmptyKind(filters, semantics, isAnyFiltered(filterCols, filters))];


  return (
    <div className="nd-ev">
      <div className="nd-ev-head">
        <Link className="nd-ev-open" to={`/events?node_id=${encodeURIComponent(node.id)}`}>
          {t('eventLog.openInEvents')} →
        </Link>
      </div>
      <TableToolbar>
        <FilterButton
          columns={filterCols}
          filters={filters}
          onOpen={() => {
            for (const c of filterCols) facets.load(c.key);
            setSheet(true);
          }}
        />
        <ClearFilters
          columns={filterCols}
          filters={filters}
          onClear={() => setFilters(defaultFilters(filterCols))}
        />
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={exhausted ? t('events.events') : t('events.eventsLoaded')}
        />
      </TableToolbar>
      {widened && <p className="ev-widened">{t('events.widened')}</p>}
      <DataTable
        tableId="node.events"
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
