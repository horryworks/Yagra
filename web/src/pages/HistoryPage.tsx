// SPDX-License-Identifier: AGPL-3.0-only
// Alert history (Alerts ▸ History). Append-only lifecycle log from /alerts/history. Each row
// is a transition (fire / clear). MTTR is open→clear in this model (§3.2: ack/response time is
// external and not measured here). Empty in skeleton mode (no persistent store). Rendered with
// the virtualized DataTable on the v2 table standard.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import {
  alertWhat,
  formatTimestamp,
  severityColorVar,
  severityLabel,
  stateLabel,
} from '../lib/format';
import { api } from '../services/api';
import { type AlertHistoryRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Badge } from '../components/ui/Badge';
import { useEntityNames } from '../components/ui/EntityName';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, isAnyFiltered, specColumns } from '../lib/columnFilter';
import { useFilterParams } from '../lib/useFilterParams';
import { AlertSubjectName } from '../widgets/AlertSubjectName';
import { AlertWhatText } from '../widgets/AlertWhatText';
import { appendPage, nextCursor } from './historyCursor';
import { historyFilters, queryFor, readScope, writeScope } from './historyQuery';
// Reused in place rather than moved: ScopePicker already answers "all / this group / this node"
// with a server-side node typeahead, which is exactly the node_id + group_id pair this screen
// filters on. It is mounted on the `troubleshoot` i18n namespace, so moving it to components/ means
// moving its strings and its consumers too — worth doing, but not inside this change.
import { ScopePicker } from '../components/ScopePicker/ScopePicker';
import { allScope, nodeScopeLabel, type ScopeValue } from '../components/ScopePicker/scope';
import { scopeFilter } from '../troubleshoot/findingsQuery';

export function HistoryPage() {
  const { t } = useTranslation('alerts');
  const [rows, setRows] = useState<AlertHistoryRow[]>([]);
  const [sheet, setSheet] = useState(false);
  const [loading, setLoading] = useState(true);
  /** Keyset cursor for the next (older) page; `null` once the log is exhausted. */
  const [cursor, setCursor] = useState<{ before: string; before_id: string } | null>(null);
  // Re-entrancy guard: DataTable fires onReachEnd on every render while the last row is in view,
  // so coalesce overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);
  const { nodeName } = useEntityNames();

  // ⚠️ **`filterCols` comes from the specs, not from `filterableColumns(columns)`.**
  // `useFilterParams` derives the filter state from whatever list it is given and the fetch effect
  // depends on that state, so the list has to be stable — and the display columns are not: they
  // close over `nodeName`, whose identity changes every time a name batch resolves. Deriving from
  // them would refetch the first page at that moment and discard every "load older" page below it.
  const specs = useMemo(() => historyFilters(t), [t]);
  const filterCols = useMemo(() => specColumns(specs), [specs]);
  // Columns close over `nodeName`, so they rebuild when the inventory resolves — which is what the
  // rendered names need and exactly what the filter list must not do.
  const columns = useMemo<Column<AlertHistoryRow>[]>(() => {
    const cols: Column<AlertHistoryRow>[] = [
      {
        key: 'severity',
        header: t('history.cols.severity'),
        width: '110px',
        render: (r) => (
          <span className="yt-status">
            <span className="yt-status-dot" style={{ background: severityColorVar(r.severity) }} />
            <span className="muted">{severityLabel(r.severity)}</span>
          </span>
        ),
      },
      {
        // ⚠️ Keyed `node_q`, not `node`: the column key **is** the filter's URL key (ADR-053
        // decision 12) and this one carries a node-*name* substring, not the id `node_id` already
        // means. Nothing type-checks the `specs[c.key]` lookup below — a mismatch here silently
        // ships a column with no filter row cell.
        key: 'node_q',
        header: t('history.cols.node'),
        width: '1.4fr',
        // A row whose subject is not a node has nothing to resolve through the inventory — see
        // `lib/alertSubject` for why reading `node` without the kind is the mistake to avoid.
        render: (r) => <AlertSubjectName alert={r} nodeName={nodeName} />,
      },
      {
        // `metric` rather than `what`: the API parameter is the metric name, and the column key is
        // the URL key.
        key: 'metric',
        header: t('history.cols.what'),
        width: '1.6fr',
        render: (r) => <AlertWhatText what={alertWhat(r)} />,
      },
      {
        key: 'state',
        header: t('history.cols.state'),
        width: '120px',
        render: (r) => stateLabel(r.state),
      },
      {
        key: 'phase',
        header: t('history.cols.event'),
        width: '100px',
        render: (r) =>
          r.resolved ? (
            <Badge tone="up">{t('history.phase.cleared')}</Badge>
          ) : (
            <Badge tone="critical">{t('history.phase.fired')}</Badge>
          ),
      },
      {
        // Read-only ack mirrored from the external tool (ADR-015) — Yagra has no ack action.
        key: 'acked',
        header: t('history.cols.acked'),
        width: '120px',
        render: (r) =>
          r.acked ? (
            <span
              className="muted"
              title={t('acked.title', {
                source: r.acked.source,
                by: r.acked.by,
                note: r.acked.note ? t('acked.note', { note: r.acked.note }) : '',
              })}
            >
              {r.acked.source}
            </span>
          ) : (
            <span className="muted">—</span>
          ),
      },
      {
        key: 'range',
        header: t('history.cols.when'),
        width: '1fr',
        render: (r) => <span className="muted">{formatTimestamp(r.at_unix_ms)}</span>,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [nodeName, specs, t]);

  // The filters live in the URL — the only source of truth for them, so a node page can deep-link
  // to "this node's alert history" and a narrowed view survives a reload and can be shared. Since
  // Inc.10 that is the shared codec: the column key **is** the query key, and these were already
  // named after the parameters this screen shipped with, so saved links still resolve.
  const [params] = useSearchParams();
  const { filters, setFilters, nowMs } = useFilterParams(filterCols);
  // The two scope ids are not columns, so they ride alongside — read here, written through
  // `setFilters`' `also` callback so a change to both is still ONE write to the query string.
  const scopeIds = useMemo(() => readScope(params), [params]);
  const filtered = isAnyFiltered(filterCols, filters) || !!scopeIds.nodeId || !!scopeIds.groupId;

  const [scope, setScope] = useState<ScopeValue>(() => allScope(t));
  const onScope = (v: ScopeValue) => {
    setScope(v);
    setFilters(filters, writeScope(scopeFilter(v)));
  };
  // A deep link arrives with an id and no label; show the id until the inventory resolves a name.
  useEffect(() => {
    if (scopeIds.nodeId && scope.kind !== 'node') {
      setScope({
        kind: 'node',
        id: scopeIds.nodeId,
        label: nodeScopeLabel(nodeName(scopeIds.nodeId) ?? scopeIds.nodeId, t),
      });
    }
    // Only on arrival: afterwards the picker owns its own label.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Refetch from the top whenever the filter changes — a cursor is only meaningful within one
  // filter, so carrying it across a change would page into the previous query's results.
  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    api
      .listAlertHistory(queryFor(filterCols, filters, scopeIds, null, nowMs))
      .then((page) => {
        if (cancelled) return;
        setRows(page);
        setCursor(nextCursor(page));
      })
      .catch(() => undefined)
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [filterCols, filters, scopeIds, nowMs]);

  // Keyset "load older": fetch the next page strictly older than the last loaded row. The log is
  // append-only and can grow without bound, so we page on scroll instead of one capped fetch.
  //
  // The cursor is the (recorded_at, id) pair, not the timestamp alone. A whole flush of alerts is
  // written in one transaction and shares one recorded_at, so the old timestamp-only cursor skipped
  // that flush's remaining rows whenever a page boundary landed inside it — silently, and most
  // often during the fleet-wide events this log exists to explain.
  const loadMore = useCallback(() => {
    if (loadingMore.current || cursor === null) return;
    loadingMore.current = true;
    api
      .listAlertHistory(queryFor(filterCols, filters, scopeIds, cursor, nowMs))
      .then((page) => {
        setRows((cur) => appendPage(cur, page));
        setCursor(nextCursor(page));
      })
      .catch(() => undefined)
      .finally(() => {
        loadingMore.current = false;
      });
  }, [cursor, filterCols, filters, scopeIds, nowMs]);


  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:alerts.history')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.history') }]}
        note={t('history.note')}
      />
      {/* Fixed toolbar order (design-system §4.1): 検索 → フィルタ → spacer → 件数 → 主アクション.
          There is no search box: the only free-text column is `metric`, unindexed on a table that
          reaches millions of rows, so an ILIKE there would turn the keyset seek into a seq scan.
          "Which node" is what ScopePicker answers instead. */}
      <TableToolbar>
        <ScopePicker value={scope} onChange={onScope} className="table-filter" />
        <FilterButton columns={filterCols} filters={filters} onOpen={() => setSheet(true)} />
        {/* The scope is counted and cleared with the columns: it is not a column filter, but it
            narrows this list, and a "clear all" that leaves a node selected is a lie. Both go into
            ONE write — the columns through `setFilters`, the two ids through its `also` callback. */}
        <ClearFilters
          columns={filterCols}
          filters={filters}
          extraActive={!!scopeIds.nodeId || !!scopeIds.groupId}
          onClear={() => {
            setScope(allScope(t));
            setFilters(defaultFilters(filterCols), writeScope({ nodeId: '', groupId: '' }));
          }}
        />
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={cursor === null ? t('history.transitions') : t('history.transitionsLoaded')}
        />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        filters={filters}
        onFiltersChange={setFilters}
        // The row's own id. The composite key this replaces was not unique — two transitions of the
        // same subject and check, in the same millisecond, collided — and a duplicate React key is
        // a silent misrender rather than an error.
        rowKey={(r) => r.id}
        onReachEnd={cursor === null ? undefined : loadMore}
        // Keyed off the filter, never off `rows.length`: with the predicate in SQL, a filtered query
        // that legitimately returns zero is indistinguishable from an empty log.
        empty={filtered ? t('history.emptyFiltered') : t('history.empty')}
        loading={loading}
        // No facet counts: every count here would be a second aggregate query over a table that
        // reaches millions of rows, per popover open. ADR-023 puts UI load third.
      />
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          labels={{
            severity: t('history.cols.severity'),
            state: t('history.cols.state'),
            phase: t('history.cols.event'),
            range: t('history.cols.when'),
          }}
          onClose={() => setSheet(false)}
        />
      )}
    </div>
  );
}
