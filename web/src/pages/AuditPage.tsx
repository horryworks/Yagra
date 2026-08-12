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
import { AUDIT_ACTIONS, AUDIT_STATUS_CLASSES, type AuditRow } from '../types/api';
import { useDebouncedValue } from '../lib/useDebouncedValue';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import {
  TableToolbar,
  SearchInput,
  TableSpacer,
  ResultCount,
  FilterSelect,
} from '../components/ui/TableToolbar';
import { TimeCell, HttpStatus, MethodChip, Monogram } from '../components/ui/tableCells';
import { DownloadIcon } from '../components/ui/icons';
import { csvField, parseAction } from './auditRow';
import {
  appendPage,
  AUDIT_RANGES,
  DEFAULT_FILTERS,
  isFiltered,
  nextCursor,
  queryFor,
  type AuditFilters,
} from './auditQuery';

/** Columns for the virtualized table. Stateless renderers, but the headers + synthetic "sign in"
 *  label are localized, so build them from the calling component's `t` (rebuild on language
 *  change). HTTP method names (POST/PUT/…) and paths are technical and rendered verbatim. */
function auditColumns(t: TFunction): Column<AuditRow>[] {
  return [
    { key: 'time', header: t('audit.cols.time'), width: '190px', render: (r) => <TimeCell iso={r.at} /> },
    {
      key: 'user',
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
  const [draftQ, setDraftQ] = useState(DEFAULT_FILTERS.q);
  const [filters, setFilters] = useState<AuditFilters>(DEFAULT_FILTERS);
  const settledQ = useDebouncedValue(draftQ);
  const active = useMemo<AuditFilters>(() => ({ ...filters, q: settledQ }), [filters, settledQ]);
  const set = <K extends keyof AuditFilters>(key: K, value: AuditFilters[K]) =>
    setFilters((f) => ({ ...f, [key]: value }));

  // Columns close over the translator, so rebuild them on a language switch.
  const columns = useMemo(() => auditColumns(t), [t]);

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

  // The export is the loaded rows, and the button says so. Every one of them matches the filter
  // now, which is a real improvement — but it is still "what has been scrolled through", not
  // "everything matching". A server-side export would need a second CSV encoder in Rust, and a
  // duplicated encoder is a duplicated security boundary: this log stores the username submitted to
  // a *failed* sign-in, so the formula neutralization in `lib/csv` is load-bearing and must stay in
  // one place. Backlogged rather than half-solved.
  const exportCsv = () => {
    const header = ['time', 'user', 'action', 'status'];
    const lines = rows.map((r) => [r.at, r.username, r.action, r.status].map(csvField).join(','));
    const csv = [header.join(','), ...lines].join('\r\n');
    const url = URL.createObjectURL(new Blob([csv], { type: 'text/csv;charset=utf-8' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = 'audit-log.csv';
    a.click();
    URL.revokeObjectURL(url);
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
            <SearchInput
              value={draftQ}
              onChange={setDraftQ}
              placeholder={t('audit.searchPlaceholder')}
              ariaLabel={t('audit.searchAria')}
            />
            {/* Options come from the `as const` arrays that are pinned to the backend enums, so a
                variant added in Rust is a compile error here rather than a filter nobody can pick.
                The method labels stay verbatim — POST/PUT/… are technical, not prose. */}
            <FilterSelect
              value={filters.action}
              onChange={(v) => set('action', v)}
              options={AUDIT_ACTIONS.map((a) => ({
                value: a,
                label: t(`audit.action.${a}`),
              }))}
              allLabel={t('audit.filter.allActions')}
              ariaLabel={t('audit.filterActionAria')}
            />
            <FilterSelect
              value={filters.status}
              onChange={(v) => set('status', v)}
              options={AUDIT_STATUS_CLASSES.map((s) => ({
                value: s,
                label: t(`audit.statusClass.${s}`),
              }))}
              allLabel={t('audit.filter.allStatus')}
              ariaLabel={t('audit.filterStatusAria')}
            />
            {/* Not a FilterSelect: the range has no "no filter" member — `all` *is* one of its
                options and its default, so the '' sentinel would be a second way to say the same
                thing. */}
            <Select
              value={filters.range}
              onChange={(e) => set('range', e.target.value as AuditFilters['range'])}
              className="table-filter"
              aria-label={t('common:range.timeRange')}
            >
              {AUDIT_RANGES.map((r) => (
                <option key={r} value={r}>
                  {t(`audit.range.${r}`)}
                </option>
              ))}
            </Select>
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
            rowKey={(r) => r.id}
            onReachEnd={cursor === null ? undefined : loadMore}
            loading={loading}
            empty={isFiltered(active) ? t('audit.empty.filtered') : t('audit.empty.none')}
          />
        </>
      )}
    </div>
  );
}
