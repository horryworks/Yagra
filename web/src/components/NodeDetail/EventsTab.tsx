// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Events tab: this node's received passive events (syslog / SNMP traps / webhooks) as
// a full keyset-paged log, reusing the shared event-log hook + columns + filter descriptors. The
// Source column and its filter are both dropped — every row is this node — and "Open in Events →"
// deep-links to the node-filtered Events page.
//
// The filters are **local state, not the URL**, and that is the one deliberate difference from the
// Events page. This tab's own identity already lives in the query string (`?tab=events`), and a
// node page's URL is shared to mean "this node", not "this node filtered like so"; putting five
// more keys there would also collide with whatever the *next* tab wants to store.

import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { NodeDetail } from '../../types/api';
import { DataTable } from '../ui/DataTable';
import { MobileFilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { TableToolbar, TableSpacer, ResultCount } from '../ui/TableToolbar';
import { useEntityNames } from '../ui/EntityName';
import { eventColumns, eventCard } from '../EventLog/eventColumns';
import {
  eventEmptyKind,
  eventFilterColumns,
  eventFilterQuery,
} from '../EventLog/eventFilterSpec';
import {
  eventColumnLabels,
  useEventFacets,
  useSearchSemantics,
  useWidenedEventLog,
} from '../EventLog/useEventFilters';
import { defaultFilters, isAnyFiltered, type FilterState } from '../../lib/columnFilter';

export function EventsTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('alerts');
  const { nodeName } = useEntityNames();
  const semantics = useSearchSemantics();
  const [sheet, setSheet] = useState(false);

  const filterCols = useMemo(
    () => eventFilterColumns(t, { showSource: false, semantics }),
    [t, semantics],
  );
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(filterCols));
  // Resolved when the range changes, never per request — a lower bound that creeps forward between
  // "load older" pages drops rows the keyset cursor was walking towards (`boundsFor`).
  const [nowMs, setNowMs] = useState(() => Date.now());
  const rangeValue = filters.at ?? '';

  const query = useMemo(() => eventFilterQuery(filters, nowMs), [filters, nowMs]);
  const facets = useEventFacets(filterCols, filters, nowMs, { node_id: node.id });

  const { rows, loading, exhausted, loadMore, widened } = useWidenedEventLog(query, semantics, {
    node_id: node.id,
  });

  const columns = useMemo(
    () => eventColumns(nodeName, t, { showSource: false, semantics }),
    [nodeName, t, semantics],
  );
  const renderCard = useMemo(() => eventCard(nodeName, t, { showSource: false }), [nodeName, t]);

  const empty = {
    unfiltered: t('eventLog.emptyNodeWindow'),
    filtered: t('eventLog.emptyNode'),
    prefixMiss: t('events.emptyPrefixMiss'),
  }[eventEmptyKind(filters, semantics, isAnyFiltered(filterCols, filters))];

  const apply = (next: FilterState) => {
    if ((next.at ?? '') !== rangeValue) setNowMs(Date.now());
    setFilters(next);
  };

  return (
    <div className="nd-ev">
      <div className="nd-ev-head">
        <Link className="nd-ev-open" to={`/alerts/events?node_id=${encodeURIComponent(node.id)}`}>
          {t('eventLog.openInEvents')} →
        </Link>
      </div>
      <TableToolbar>
        <MobileFilterButton
          columns={filterCols}
          filters={filters}
          onOpen={() => {
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
      {widened && <p className="ev-widened">{t('events.widened')}</p>}
      <DataTable
        rows={rows}
        columns={columns}
        filters={filters}
        onFiltersChange={apply}
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
          onChange={apply}
          counts={facets.counts}
          labels={eventColumnLabels(t)}
          onClose={() => setSheet(false)}
        />
      )}
    </div>
  );
}
