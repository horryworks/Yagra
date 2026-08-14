// SPDX-License-Identifier: AGPL-3.0-only
// Audit log (Settings ▸ Audit log). Admin-only: every mutating API call (who, what, when,
// response status) plus login events. Read-only & immutable, newest-first, keyset paging — the
// log is append-only and can grow without bound.
//
// Data-table standard v2: a toolbar (search + action/status/time-range filters + count + Export)
// over the shared virtualized `DataTable` (windowed for tens of thousands of rows, §4). No row
// actions (immutable); the primary action is Export, not Add.
//
// **The filters are server-side.** They used to narrow the already-loaded pages in this file, which
// made the toolbar lie: "last 30 days, DELETE only" examined the newest 100 rows and silently hid
// every older match, and Export handed the operator that same partial set. In a log whose purpose is
// completeness that is a correctness bug. The controls and their wording are unchanged; what moved
// is where the predicate runs (`pages/auditQuery.ts` → `GET /api/v1/audit` → SQL).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { AuditRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { filterableColumns, type FilterState } from '../lib/columnFilter';
import { TimeCell, HttpStatus, MethodChip, Monogram } from '../components/ui/tableCells';
import { DownloadIcon } from '../components/ui/icons';
import { parseAction } from './auditRow';
import {
  appendPage,
  auditFilters,
  DEFAULT_FILTERS,
  exportUrl,
  filtersFromState,
  isFiltered,
  nextCursor,
  queryFor,
  stateFromFilters,
  type AuditFilters,
} from './auditQuery';

/** Columns for the virtualized table. Stateless renderers, but the headers + synthetic "sign in"
 *  label are localized, so build them from the calling component's `t` (rebuild on language
 *  change). HTTP method names (POST/PUT/…) and paths are technical and rendered verbatim. */
function auditColumns(t: TFunction): Column<AuditRow>[] {
  const specs = auditFilters(t);
  const cols: Column<AuditRow>[] = [
    { key: 'range', header: t('audit.cols.time'), width: '190px', render: (r) => <TimeCell iso={r.at} /> },
    {
      key: 'q',
      header: t('audit.cols.user'),
      width: '168px',
      render: (r) => (
        <span className={r.username === 'unknown' ? 'yt-user system' : 'yt-user'}>
          <Monogram name={r.username} system={r.username === 'unknown'} />
          <span className="yt-user-name">{r.username}</span>
        </span>
      ),
    },
    {
      key: 'action',
      header: t('audit.cols.action'),
      width: '1fr',
      render: (r) => {
        const a = parseAction(r.action);
        return a.login ? (
          <MethodChip label={t('audit.signIn')} />
        ) : (
          <>
            <MethodChip label={a.method} />
            <span className="yt-path">{a.path}</span>
          </>
        );
      },
    },
    { key: 'status', header: t('audit.cols.status'), width: '150px', render: (r) => <HttpStatus status={r.status} /> },
  ];
  for (const c of cols) c.filter = specs[c.key];
  return cols;
}

export function AuditPage() {
  const { t } = useTranslation('access');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<AuditRow[]>([]);
  /** Keyset cursor for the next (older) page; `null` once the filtered query is exhausted. */
  const [cursor, setCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Re-entrancy guard: DataTable fires onReachEnd on every render while the last row is in view,
  // so coalesce overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);

  // The search box settles before it is sent; the selects commit immediately (picking an option is
  // already a deliberate act, and waiting on it would feel broken).
  const [sheet, setSheet] = useState(false);
  // The filter row's flat state. The debounce that used to live here is inside
  // `TextConditionEditor` now — one implementation instead of one per screen.
  const [rowFilters, setRowFilters] = useState<FilterState>(() =>
    stateFromFilters(DEFAULT_FILTERS),
  );
  const active = useMemo<AuditFilters>(() => filtersFromState(rowFilters), [rowFilters]);

  // Columns close over the translator, so rebuild them on a language switch.
  const columns = useMemo(() => auditColumns(t), [t]);
  const filterCols = useMemo(() => filterableColumns(columns), [columns]);

  // Refetch from the top whenever the filter changes — the cursor is only meaningful within one
  // filter, so carrying it across a change would page into the previous query's results.
  useEffect(() => {
    if (!authed) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    api
      .listAudit(queryFor(active, null, Date.now()))
      .then((page) => {
        if (cancelled) return;
        setRows(page);
        setCursor(nextCursor(page));
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof ApiError ? e.message : t('audit.err.load'));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [authed, active, t]);

  const loadMore = useCallback(() => {
    if (loadingMore.current || cursor === null) return;
    loadingMore.current = true;
    api
      .listAudit(queryFor(active, cursor, Date.now()))
      .then((page) => {
        setRows((cur) => appendPage(cur, page));
        setCursor(nextCursor(page));
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : t('audit.err.loadMore')))
      .finally(() => {
        loadingMore.current = false;
      });
  }, [active, cursor, t]);

  // The export is now everything matching the filter, not the rows that happen to be loaded — the
  // server renders it (`GET /api/v1/audit/export.csv`) with the same filter this page is showing.
  //
  // The objection that kept it client-side was that a Rust CSV encoder would be a *second* one, and
  // a duplicated encoder is a duplicated security boundary: this log stores the username submitted
  // to a **failed** sign-in, so formula neutralization is load-bearing. That was the right concern
  // and the wrong conclusion — the backend already had a CSV encoder (report exports) and it did
  // **not** neutralize. There were two all along, and one was broken. `crates/yagra-core/src/csv.rs`
  // is now the single one, mirrored against `lib/csv.ts` by a test.
  //
  // Navigating rather than fetching: the browser's own download handles the file, so a 50k-row
  // export never passes through JS memory, and the filename comes from Content-Disposition.
  const exportCsv = () => {
    window.location.assign(exportUrl(active, Date.now()));
  };

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:settings.audit')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.audit') }]}
        note={t('audit.note')}
      />

      {!authed ? (
        <Card>
          <p className="muted">{t('audit.signInPrompt')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={rowFilters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters
              columns={filterCols}
              filters={rowFilters}
              onClear={() => setRowFilters(stateFromFilters(DEFAULT_FILTERS))}
            />
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('audit.entry', { count: rows.length })} />
            <Button
              variant="outline"
              onClick={exportCsv}
              disabled={rows.length === 0}
              title={t('audit.exportHint')}
            >
              <DownloadIcon width={15} height={15} /> {t('audit.export')}
            </Button>
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          {/* Keyed off the filter, never off `rows.length`: with the predicate in SQL, a filtered
              query that legitimately returns zero is indistinguishable from an empty log, and the
              screen would claim there is nothing here while a filter is hiding it. */}
          <DataTable
            rows={rows}
            columns={columns}
            filters={rowFilters}
            onFiltersChange={setRowFilters}
            rowKey={(r) => r.id}
            onReachEnd={cursor === null ? undefined : loadMore}
            loading={loading}
            empty={isFiltered(active) ? t('audit.empty.filtered') : t('audit.empty.none')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={rowFilters}
              onChange={setRowFilters}
              labels={{
                q: t('audit.cols.user'),
                action: t('audit.cols.action'),
                status: t('audit.cols.status'),
                range: t('audit.cols.time'),
              }}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}
    </div>
  );
}
