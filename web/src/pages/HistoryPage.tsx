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
import { SEVERITIES, type AlertHistoryRow } from '../types/api';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { PageHeader } from '../components/ui/PageHeader';
import { Badge } from '../components/ui/Badge';
import { useEntityNames } from '../components/ui/EntityName';
import { DataTable, type Column } from '../components/ui/DataTable';
import {
  TableToolbar,
  TableSpacer,
  ResultCount,
  FilterSelect,
} from '../components/ui/TableToolbar';
import { Select } from '../components/ui/Field';
import { AlertSubjectName } from '../widgets/AlertSubjectName';
import { AlertWhatText } from '../widgets/AlertWhatText';
import { appendPage, nextCursor } from './historyCursor';
import {
  HISTORY_RANGES,
  isFiltered,
  queryFor,
  readFilters,
  writeFilters,
  type HistoryFilters,
} from './historyQuery';
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
  const [loading, setLoading] = useState(true);
  /** Keyset cursor for the next (older) page; `null` once the log is exhausted. */
  const [cursor, setCursor] = useState<{ before: string; before_id: string } | null>(null);
  // Re-entrancy guard: DataTable fires onReachEnd on every render while the last row is in view,
  // so coalesce overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);
  const { nodeName } = useEntityNames();

  // The filters live in the URL — the only source of truth for them, so a node page can deep-link
  // to "this node's alert history" and a narrowed view survives a reload and can be shared.
  // `replace: true` because a settled filter is not a place you navigated to: pushing one per
  // change would make Back walk through every intermediate state instead of leaving the screen.
  const [params, setParams] = useSearchParams();
  const filters = useMemo(
    () => readFilters(params, SEVERITIES, SEVERITY_ORDER),
    [params],
  );
  const setFilters = (next: HistoryFilters) => {
    const p = new URLSearchParams(params);
    writeFilters(p, next);
    setParams(p, { replace: true });
  };
  const set = <K extends keyof HistoryFilters>(key: K, value: HistoryFilters[K]) =>
    setFilters({ ...filters, [key]: value });

  const [scope, setScope] = useState<ScopeValue>(() => allScope(t));
  const onScope = (v: ScopeValue) => {
    setScope(v);
    setFilters({ ...filters, ...scopeFilter(v) });
  };
  // A deep link arrives with an id and no label; show the id until the inventory resolves a name.
  useEffect(() => {
    if (filters.nodeId && scope.kind !== 'node') {
      setScope({
        kind: 'node',
        id: filters.nodeId,
        label: nodeScopeLabel(nodeName(filters.nodeId) ?? filters.nodeId, t),
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
      .listAlertHistory(queryFor(filters, null, Date.now()))
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
  }, [filters]);

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
      .listAlertHistory(queryFor(filters, cursor, Date.now()))
      .then((page) => {
        setRows((cur) => appendPage(cur, page));
        setCursor(nextCursor(page));
      })
      .catch(() => undefined)
      .finally(() => {
        loadingMore.current = false;
      });
  }, [cursor, filters]);

  // Columns close over `nodeName`, so rebuild them when the inventory resolves.
  const columns = useMemo<Column<AlertHistoryRow>[]>(
    () => [
      {
        key: 'sev',
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
        key: 'node',
        header: t('history.cols.node'),
        width: '1.4fr',
        // A row whose subject is not a node has nothing to resolve through the inventory — see
        // `lib/alertSubject` for why reading `node` without the kind is the mistake to avoid.
        render: (r) => <AlertSubjectName alert={r} nodeName={nodeName} />,
      },
      {
        key: 'what',
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
        key: 'at',
        header: t('history.cols.when'),
        width: '1fr',
        render: (r) => <span className="muted">{formatTimestamp(r.at_unix_ms)}</span>,
      },
    ],
    [nodeName, t],
  );

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
        <FilterSelect
          value={filters.severity}
          onChange={(v) => set('severity', v)}
          options={SEVERITIES.map((s) => ({ value: s, label: severityLabel(s) }))}
          allLabel={t('history.filter.allSeverities')}
          ariaLabel={t('history.filter.severityAria')}
        />
        <FilterSelect
          value={filters.phase}
          onChange={(v) => set('phase', v)}
          options={[
            { value: 'fired' as const, label: t('history.phase.fired') },
            { value: 'cleared' as const, label: t('history.phase.cleared') },
          ]}
          allLabel={t('history.filter.allPhases')}
          ariaLabel={t('history.filter.phaseAria')}
        />
        <FilterSelect
          value={filters.state}
          onChange={(v) => set('state', v)}
          options={SEVERITY_ORDER.map((s) => ({ value: s, label: stateLabel(s) }))}
          allLabel={t('history.filter.allStates')}
          ariaLabel={t('history.filter.stateAria')}
        />
        {/* Not a FilterSelect: `all` is one of the range's own options and its default, so the ''
            sentinel would be a second way to say the same thing. */}
        <Select
          value={filters.range}
          onChange={(e) => set('range', e.target.value as HistoryFilters['range'])}
          className="table-filter"
          aria-label={t('common:range.timeRange')}
        >
          {HISTORY_RANGES.map((r) => (
            <option key={r} value={r}>
              {t(`history.range.${r}`)}
            </option>
          ))}
        </Select>
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={cursor === null ? t('history.transitions') : t('history.transitionsLoaded')}
        />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        // The row's own id. The composite key this replaces was not unique — two transitions of the
        // same subject and check, in the same millisecond, collided — and a duplicate React key is
        // a silent misrender rather than an error.
        rowKey={(r) => r.id}
        onReachEnd={cursor === null ? undefined : loadMore}
        // Keyed off the filter, never off `rows.length`: with the predicate in SQL, a filtered query
        // that legitimately returns zero is indistinguishable from an empty log.
        empty={isFiltered(filters) ? t('history.emptyFiltered') : t('history.empty')}
        loading={loading}
      />
    </div>
  );
}
