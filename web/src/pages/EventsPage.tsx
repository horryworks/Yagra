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
import { Trans, useTranslation } from 'react-i18next';
import { api } from '../services/api';
import { PageHeader } from '../components/ui/PageHeader';
import { useEntityNames } from '../components/ui/entityNames';
import { DataTable } from '../components/ui/DataTable';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { ScreenLink } from '../components/ui/ScreenLink';
import { ListToolbar } from '../components/ui/ListToolbar';
import { serverToolbarFilters } from '../lib/listToolbar';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { eventColumns, eventCard } from '../components/EventLog/eventColumns';
import {
  eventEmptyKind,
  eventFilterColumns,
  eventFilterQuery,
  eventHighlight,
  prefixMissTerm,
  reachesPastDefaultWindow,
  widenedToAWeek,
} from '../components/EventLog/eventFilterSpec';
import {
  eventColumnLabels,
  useEventFacets,
  useWidenedEventLog,
  useSearchSemantics,
} from '../components/EventLog/useEventFilters';
import { eventListeners, type ListenerBinding } from '../components/EventLog/listeners';
import { useFilterParams } from '../lib/useFilterParams';
import { isAnyFiltered } from '../lib/columnFilter';
import { readIdParam, writeIdParam } from '../lib/filterParams';

/**
 * Where the fleet is currently listening for syslog and traps (ADR-055 decision 3).
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
  // Each empty state is one sentence and, where there is one, the next step (ADR-200): an empty
  // default window offers the wider one rather than describing where the range control is.
  const empty = {
    unfiltered: (
      <EmptyState
        text={t('events.emptyWindow')}
        action={
          <Button type="button" onClick={() => setFilters(widenedToAWeek(filters))}>
            {t('events.showWeek')}
          </Button>
        }
      />
    ),
    filtered: t('common:filter.noMatch'),
    prefixMiss: t('events.emptyPrefixMiss', { term: prefixMissTerm(filters) }),
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
      />
      {/* The answer to "where do I point my devices". It lives on this screen rather than beside
          the webhook list because this is where someone who sees no syslog comes looking — and
          because `Webhook sources` no longer claims to cover it (ADR-055 decision 3 / R1). Rendered
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
      {/* The node picker is counted and cleared with the columns: it is not a column filter, but
          it narrows this list, and "clear all filters" that leaves a node selected is a lie.
          ⚠️ Both go into ONE `setSearchParams` — `serverToolbarFilters` folds the picker's reset
          into the columns' write (`setFilters`' `also` says what happened when they were two). */}
      <ListToolbar
        list={serverToolbarFilters(
          filterCols,
          { filters, setFilters },
          { active: nodeId != null, clear: (p) => writeIdParam(p, 'node_id', null) },
          facets.counts,
        )}
        labels={eventColumnLabels(t)}
        // The sheet shows every column at once, so its counts are fetched together rather than
        // per popover — there is no "opened this one" signal on mobile.
        onSheetOpen={() => {
          for (const c of filterCols) facets.load(c.key);
        }}
        count={{
          shown: rows.length,
          noun: () => (exhausted ? t('events.events') : t('events.eventsLoaded')),
        }}
        leading={
          <NodePicker
            value={nodeId ?? null}
            valueLabel={nodeId ? nodeName(nodeId) : undefined}
            onChange={setNode}
            placeholder={t('nav:nodes.all')}
          />
        }
      />
      {/* Said out loud, because the rows below are the answer to a slightly broader question than
          the one the operator typed. Silently widening would be the worse half of this trade. */}
      {widened && <p className="ev-widened">{t('events.widened')}</p>}
      {/* What the page note used to say on every visit, said only when it is true of the rows:
          beyond the default window the list can be missing the events that matched no rule. */}
      {reachesPastDefaultWindow(filters, nowMs) && (
        <p className="ev-retention">
          <Trans
            t={t}
            i18nKey="events.unmatchedKept"
            components={{ lnk: <ScreenLink to="/settings/system" /> }}
          />
        </p>
      )}
      <DataTable
        tableId="events.log"
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
