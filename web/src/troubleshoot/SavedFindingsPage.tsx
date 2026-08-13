// SPDX-License-Identifier: AGPL-3.0-only
// Saved findings (`/troubleshoot/findings`) — everything the analyses have found, across runs.
//
// The runs list next door answers "what did I run"; this answers the question an operator actually
// starts from — "has anything been found about this node / this site / this week" — which no single
// run can answer, because otherwise finding out means opening runs one at a time.
//
// Data-table standard v2: a toolbar (scope + tool + severity + range + count) over the shared
// virtualized `DataTable`, keyset-paged (ADR-019) — the findings table only grows. Every decision
// about *what is asked for* lives in `findingsQuery.ts`, because Vitest does not run `.tsx`.
//
// Times come from `at` through `TimeCell`, never from the backend's `when_label` / `duration`:
// those are pre-rendered English (`format!("{cycles} cycles")`) and would render English under JA
// — the report bodies treat them as a fallback for the same reason. The row links to the run's
// report, which is where the structured detail lives.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import { FINDING_SEVERITIES, type SavedFinding } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { filterableColumns, type FilterState } from '../lib/columnFilter';
import { TimeCell } from '../components/ui/tableCells';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { ScopePicker } from '../components/ScopePicker/ScopePicker';
import { allScope, type ScopeValue } from '../components/ScopePicker/scope';
import { reportPathFor, toolById } from './data';
import { sevOf } from './report/format';
import {
  DEFAULT_FILTERS,
  appendPage,
  findingFilters,
  filtersFromState,
  isFiltered,
  nextCursor,
  queryFor,
  scopeFilter,
  stateFromFilters,
  type FindingCursor,
  type FindingFilters,
} from './findingsQuery';
import './troubleshoot.css';

/** Status colour per severity bucket — the domain's canonical set, never an ad-hoc red/amber. */
const SEV_COLOR: Record<(typeof FINDING_SEVERITIES)[number], string> = {
  crit: 'var(--status-critical)',
  warn: 'var(--status-warning)',
  info: 'var(--text-tertiary)',
};

/** Columns for the virtualized table. Built from the caller's `t` so a language switch rebuilds
 *  the headers and the tool names. */
function findingColumns(t: TFunction, nodeName: (id: string) => string): Column<SavedFinding>[] {
  const specs = findingFilters(t);
  const cols: Column<SavedFinding>[] = [
    {
      key: 'severity',
      header: t('findings.cols.severity'),
      width: '118px',
      render: (f) => {
        const sev = sevOf(f);
        return (
          <span className="ts-sf-sev">
            <span className="ts-sf-dot" style={{ background: SEV_COLOR[sev] }} />
            {t(`findings.severity.${sev}`)}
          </span>
        );
      },
    },
    {
      key: 'score',
      header: t('findings.cols.score'),
      width: '78px',
      align: 'right',
      render: (f) => <span className="mono">{Math.round(f.score)}</span>,
    },
    {
      // ⚠️ Keyed `node_q`, not `node`: the column key **is** the filter's URL key (ADR-053
      // decision 12) and this one carries a node-*name* substring, not the id `node_id` means.
      // Nothing type-checks the `specs[c.key]` lookup below.
      key: 'node_q',
      header: t('findings.cols.node'),
      width: '1.2fr',
      // Resolved through `useEntityNames`, not from the row's own `node_name`: that name was
      // denormalised when the run finished and a node renamed since would show the old one.
      // A fleet-level finding has no node to resolve, so it keeps the label the run wrote.
      render: (f) =>
        f.node_id ? (
          <EntityName name={nodeName(f.node_id)} id={f.node_id} />
        ) : (
          <span className="muted">{f.node_name || t('findings.fleetWide')}</span>
        ),
    },
    {
      // `q` rather than `what`: the API parameter matches the metric **and** the kind, which is
      // exactly the pair this column renders.
      key: 'q',
      header: t('findings.cols.what'),
      width: '1.6fr',
      render: (f) => (
        <>
          <span className="mono ts-sf-metric">{f.metric}</span>
          <span className="ts-sf-kind">{f.kind}</span>
        </>
      ),
    },
    {
      key: 'tool',
      header: t('findings.cols.tool'),
      width: '180px',
      // `tool` is a bare string on the wire, so the catalog lookup is also what narrows it; a run
      // from a newer core naming a tool this build lacks shows its raw key rather than nothing.
      render: (f) => {
        const tool = toolById(f.tool);
        return <span>{tool ? t(tool.name) : f.tool}</span>;
      },
    },
    {
      key: 'range',
      header: t('findings.cols.at'),
      width: '190px',
      render: (f) => <TimeCell iso={f.at} />,
    },
  ];
  for (const c of cols) c.filter = specs[c.key];
  return cols;
}

export function SavedFindingsPage() {
  const { t } = useTranslation('troubleshoot');
  const navigate = useNavigate();
  const authed = useAuthStore((s) => s.authed);
  const { nodeName } = useEntityNames();

  const [filters, setFilters] = useState<FindingFilters>(DEFAULT_FILTERS);
  const [scope, setScope] = useState<ScopeValue>(() => allScope(t));
  const [sheet, setSheet] = useState(false);
  const [rows, setRows] = useState<SavedFinding[]>([]);
  const [cursor, setCursor] = useState<FindingCursor | null>(null);
  const [exhausted, setExhausted] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // DataTable fires onReachEnd on every render while the last row is in view, so coalesce
  // overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);

  const columns = useMemo(() => findingColumns(t, nodeName), [t, nodeName]);
  const filterCols = useMemo(() => filterableColumns(columns), [columns]);
  const rowFilters = useMemo(() => stateFromFilters(filters), [filters]);
  const onRowFilters = (next: FilterState) =>
    setFilters(filtersFromState(next, { nodeId: filters.nodeId, groupId: filters.groupId }));

  // First page — refetched whenever a filter changes, because filtering happens server-side (the
  // table is keyset-paged, so a client-side filter would only ever narrow the pages already
  // loaded and quietly hide older matches).
  useEffect(() => {
    if (!authed) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .searchFindings(queryFor(filters, null, Date.now()))
      .then((page) => {
        if (cancelled) return;
        setRows(page);
        setCursor(nextCursor(page));
        setExhausted(nextCursor(page) === null);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errMsg(e, t('findings.err.load')));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [authed, filters, t]);

  const loadMore = useCallback(() => {
    if (loadingMore.current || exhausted || !cursor) return;
    loadingMore.current = true;
    api
      .searchFindings(queryFor(filters, cursor, Date.now()))
      .then((page) => {
        setRows((cur) => appendPage(cur, page));
        const next = nextCursor(page);
        setCursor(next);
        setExhausted(next === null);
      })
      .catch((e: unknown) => setError(errMsg(e, t('findings.err.loadMore'))))
      .finally(() => {
        loadingMore.current = false;
      });
  }, [cursor, exhausted, filters, t]);

  const open = (f: SavedFinding) => {
    const tool = toolById(f.tool);
    // A tool this build doesn't know has no report to open; the catalog is where
    // `/troubleshoot/report/<unknown>` redirects anyway.
    const path = tool ? reportPathFor(tool.id) : '/troubleshoot';
    navigate(`${path}?job=${encodeURIComponent(f.job_id)}`);
  };

  const pickScope = (v: ScopeValue) => {
    setScope(v);
    setFilters((f) => ({ ...f, ...scopeFilter(v) }));
  };

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:troubleshoot.findings')}
        trail={[
          { label: t('nav:sections.troubleshoot'), to: '/troubleshoot' },
          { label: t('nav:troubleshoot.findings') },
        ]}
        note={t('findings.pageNote')}
      />

      {!authed ? (
        <Card>
          <p className="muted">{t('findings.signInPrompt')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <ScopePicker value={scope} onChange={pickScope} className="ts-sf-scope" />
            <MobileFilterButton
              columns={filterCols}
              filters={rowFilters}
              onOpen={() => setSheet(true)}
            />
            {/* The scope narrows this list too, so it is counted and cleared with the columns —
                a "clear all" that leaves a node selected is a lie. Both go into one write. */}
            <ClearFilters
              columns={filterCols}
              filters={rowFilters}
              extraActive={filters.nodeId !== '' || filters.groupId !== ''}
              onClear={() => {
                setScope(allScope(t));
                setFilters(DEFAULT_FILTERS);
              }}
            />
            <TableSpacer />
            <ResultCount
              shown={rows.length}
              noun={t('findings.finding', { count: rows.length })}
            />
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={rows}
            columns={columns}
            filters={rowFilters}
            onFiltersChange={onRowFilters}
            rowKey={(f) => f.id}
            onRowClick={open}
            onReachEnd={exhausted ? undefined : loadMore}
            loading={loading}
            empty={isFiltered(filters) ? t('findings.empty.filtered') : t('findings.empty.none')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={rowFilters}
              onChange={onRowFilters}
              labels={{
                severity: t('findings.cols.severity'),
                tool: t('findings.cols.tool'),
                range: t('findings.cols.at'),
              }}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}
    </div>
  );
}
