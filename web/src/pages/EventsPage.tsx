// SPDX-License-Identifier: AGPL-3.0-only
// Events (Events ▸ Events). Append-only log of received passive events (syslog / SNMP traps /
// webhooks), keyset-paged newest-first. The rule-authoring surface: browse what devices actually
// send, then write rules against it.
//
// **Every filter lives in the URL** (ADR-053). It used to be one param (`node_id`) plus five pieces
// of component state, so a link to "trap events mentioning BGP in the last week" could not be sent
// to anyone. The column keys are the URL keys — see `lib/columnFilter.ts` for why there is no
// prefix — and a filter at its default deletes its key, so a bare `/events` is always the
// default view. Fetch/paging, columns and the filter descriptors are shared with the NodeDetail
// Events tab via components/EventLog. Empty in skeleton mode.

import { useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
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
import { eventListeners, type ListenerBinding } from '../components/EventLog/listeners';
import { useFilterParams } from '../lib/useFilterParams';
import { defaultFilters, isAnyFiltered } from '../lib/columnFilter';
import { ClearFilters } from '../components/ui/ClearFilters';
import { readIdParam, writeIdParam } from '../lib/filterParams';

/**
 * Where the fleet is currently listening for syslog and traps (ADR-055 決定 3).
 *
 * `undefined` until the fetch settles, and the caller renders nothing until then — "no listener is
 * bound" is a claim, and making it before the answer arrives states the opposite of the truth for
 * the first paint of a perfectly healthy deployment.
 *
 * Best-effort by design: `GET /api/v1/pollers` needs only View, but it 503s as `admin_unavailable`
 * on a core without admin state. A failure leaves the line off. The event log is the screen; this
 * is context, and context must never be able to take the screen down with it.
 */
function useEventListeners(): ListenerBinding[] | undefined {
  const [bindings, setBindings] = useState<ListenerBinding[] | undefined>();
  useEffect(() => {
    let live = true;
    api
      .listPollers()
      .then((res) => {
        if (live) setBindings(eventListeners(res.pollers));
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);
  return bindings;
}

export function EventsPage() {
  const { t } = useTranslation('alerts');
  const [searchParams, setSearchParams] = useSearchParams();
  const nodeId = readIdParam(searchParams, 'node_id');
  const { nodeName } = useEntityNames();
  const semantics = useSearchSemantics();
  const [sheet, setSheet] = useState(false);
  const bindings = useEventListeners();

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
        title={t('nav:events.all')}
        trail={[{ label: t('nav:sections.events') }, { label: t('nav:events.all') }]}
        note={t('events.note')}
      />
      {/* The answer to "where do I point my devices". It lives on this screen rather than beside
          the webhook list because this is where someone who sees no syslog comes looking — and
          because `Webhook sources` no longer claims to cover it (ADR-055 決定 3 / R1). Rendered
          only once the fetch settles: saying "nothing is bound" before the answer arrives would be
          false on a healthy deployment's first paint. */}
      {bindings !== undefined &&
        (bindings.length === 0 ? (
          <p className="ev-listening none">{t('events.listeningNone')}</p>
        ) : (
          <p className="ev-listening">
            {t('events.listening', {
              endpoints: bindings
                .map((b) => `${b.kind} ${b.bind} (${b.pollers.join(', ')})`)
                .join(' · '),
            })}
          </p>
        ))}
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
