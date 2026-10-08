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

import { useMemo } from 'react';
import { useFilterParams } from '../../lib/useFilterParams';
import { nodeTabFilterPrefix } from './tabs';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { NodeDetail } from '../../types/api';
import { DataTable } from '../ui/DataTable';
import { Button } from '../ui/Button';
import { EmptyState } from '../ui/EmptyState';
import { ListToolbar } from '../../components/ui/ListToolbar';
import { serverToolbarFilters } from '../../lib/listToolbar';
import { useEntityNames } from '../ui/entityNames';
import { eventColumns, eventCard } from '../EventLog/eventColumns';
import {
  eventEmptyKind,
  eventFilterColumns,
  eventFilterQuery,
  eventHighlight,
  insideWordsMissTerm,
  prefixMissTerm,
  widenedToAWeek,
} from '../EventLog/eventFilterSpec';
import {
  eventColumnLabels,
  useEventFacets,
  useSearchSemantics,
  useWidenedEventLog,
} from '../EventLog/useEventFilters';
import { isAnyFiltered } from '../../lib/columnFilter';

export function EventsTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('alerts');
  const { nodeName } = useEntityNames();
  const semantics = useSearchSemantics();

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

  const { rows, loading, exhausted, loadMore, widened, searchedInsideWords } = useWidenedEventLog(
    query,
    semantics,
    { node_id: node.id },
  );

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

  // `filtered` is the generic sentence on purpose: "no events received from this node yet" is
  // false the moment a filter is set, and this tab has no Source column, so `prefixMiss` (a Source
  // term's whole-word miss) cannot occur here — it is listed only because the map is exhaustive.
  // `insideWordsMiss` can: the Message column is here, and its widened search runs here too.
  const empty = {
    unfiltered: (
      <EmptyState
        text={t('eventLog.emptyNodeWindow')}
        action={
          <Button type="button" onClick={() => setFilters(widenedToAWeek(filters))}>
            {t('events.showWeek')}
          </Button>
        }
      />
    ),
    filtered: t('common:filter.noMatch'),
    prefixMiss: t('events.emptyPrefixMiss', { term: prefixMissTerm(filters) }),
    insideWordsMiss: t('events.emptyInsideWordsMiss', { term: insideWordsMissTerm(filters) }),
  }[eventEmptyKind(filters, semantics, isAnyFiltered(filterCols, filters), searchedInsideWords)];

  return (
    <div className="nd-ev">
      <div className="nd-ev-head">
        <Link className="nd-ev-open" to={`/events?node_id=${encodeURIComponent(node.id)}`}>
          {t('eventLog.openInEvents')} →
        </Link>
      </div>
      <ListToolbar
        list={serverToolbarFilters(filterCols, { filters, setFilters }, undefined, facets.counts)}
        labels={eventColumnLabels(t)}
        onSheetOpen={() => {
          for (const c of filterCols) facets.load(c.key);
        }}
        count={{
          shown: rows.length,
          noun: () => (exhausted ? t('events.events') : t('events.eventsLoaded')),
        }}
      />
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
    </div>
  );
}
