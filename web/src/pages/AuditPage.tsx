// Audit log (Settings ▸ Audit log). Admin-only: every mutating API call (who, what, when,
// response status) plus login events. Read-only & immutable, newest-first, keyset paging — the
// log is append-only and can grow without bound.
//
// Data-table standard v2: a toolbar (search + action/status/time-range filters + count + Export)
// over the shared virtualized `DataTable` (windowed for tens of thousands of rows, §4). No row
// actions (immutable); the primary action is Export, not Add. Filtering is client-side over the
// loaded pages (the server query stays limit+keyset); scrolling to the end loads the next page.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { AuditRow } from '../types/api';
import { httpStatusTone } from '../lib/format';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TimeCell, HttpStatus, MethodChip, Monogram } from '../components/ui/tableCells';
import { DownloadIcon } from '../components/ui/icons';

const PAGE_SIZE = 100;

/** Window (ms) for the time-range filter; `0` = all loaded. */
const RANGE_MS: Record<string, number> = {
  '24h': 86_400_000,
  '7d': 7 * 86_400_000,
  '30d': 30 * 86_400_000,
  all: 0,
};

interface ParsedAction {
  method: string;
  path: string | null;
  login: boolean;
}

function parseAction(action: string): ParsedAction {
  if (action === 'auth.login') return { method: 'SIGN IN', path: null, login: true };
  const sp = action.indexOf(' ');
  if (sp < 0) return { method: action, path: null, login: false };
  return { method: action.slice(0, sp), path: action.slice(sp + 1), login: false };
}

/** Quote a CSV field (RFC 4180): wrap in quotes, double any embedded quote. */
const csvField = (v: string | number) => `"${String(v).replace(/"/g, '""')}"`;

/** Columns for the virtualized table. Stateless renderers → a stable module constant. */
const COLUMNS: Column<AuditRow>[] = [
  { key: 'time', header: 'Time', width: '190px', render: (r) => <TimeCell iso={r.at} /> },
  {
    key: 'user',
    header: 'User',
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
    header: 'Action',
    width: '1fr',
    render: (r) => {
      const a = parseAction(r.action);
      return a.login ? (
        <MethodChip label="SIGN IN" />
      ) : (
        <>
          <MethodChip label={a.method} />
          <span className="yt-path">{a.path}</span>
        </>
      );
    },
  },
  { key: 'status', header: 'Status', width: '150px', render: (r) => <HttpStatus status={r.status} /> },
];

export function AuditPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<AuditRow[]>([]);
  const [exhausted, setExhausted] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Re-entrancy guard: DataTable fires onReachEnd on every render while the last row is in view,
  // so coalesce overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);

  const [query, setQuery] = useState('');
  const [methodF, setMethodF] = useState('all');
  const [statusF, setStatusF] = useState('all');
  const [rangeF, setRangeF] = useState('all');

  const loadFirst = useCallback(() => {
    setError(null);
    api
      .listAudit({ limit: PAGE_SIZE })
      .then((page) => {
        setRows(page);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch((e: unknown) =>
        setError(e instanceof ApiError ? e.message : 'failed to load the audit log'),
      )
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    if (authed) loadFirst();
  }, [authed, loadFirst]);

  const loadMore = useCallback(() => {
    if (loadingMore.current || exhausted) return;
    const last = rows[rows.length - 1];
    if (!last) return;
    loadingMore.current = true;
    api
      .listAudit({ limit: PAGE_SIZE, before: last.at })
      .then((page) => {
        setRows((cur) => [...cur, ...page]);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to load more'))
      .finally(() => {
        loadingMore.current = false;
      });
  }, [rows, exhausted]);

  const list = useMemo(() => {
    const q = query.trim().toLowerCase();
    const cutoff = RANGE_MS[rangeF] ? Date.now() - RANGE_MS[rangeF] : 0;
    return rows.filter((r) => {
      const a = parseAction(r.action);
      const matchesQuery = q === '' || `${r.username} ${r.action}`.toLowerCase().includes(q);
      const matchesMethod =
        methodF === 'all' || (methodF === 'login' ? a.login : a.method === methodF);
      const tone = httpStatusTone(r.status);
      const matchesStatus =
        statusF === 'all' ||
        (statusF === 'ok' ? tone === 'up' : statusF === 'client' ? tone === 'warning' : tone === 'critical');
      const matchesRange = cutoff === 0 || new Date(r.at).getTime() >= cutoff;
      return matchesQuery && matchesMethod && matchesStatus && matchesRange;
    });
  }, [rows, query, methodF, statusF, rangeF]);

  const exportCsv = () => {
    const header = ['time', 'user', 'action', 'status'];
    const lines = list.map((r) => [r.at, r.username, r.action, r.status].map(csvField).join(','));
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
        title="Audit log"
        trail={[{ label: 'Settings' }, { label: 'Audit log' }]}
        note="Every configuration change and login event: who, what, when. Read-only · immutable · 365-day retention."
      />

      {!authed ? (
        <Card>
          <p className="muted">Sign in as an admin to view the audit log.</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search user or path…"
              ariaLabel="Search audit log"
            />
            <Select value={methodF} onChange={(e) => setMethodF(e.target.value)} aria-label="Filter by action">
              <option value="all">All actions</option>
              <option value="POST">POST</option>
              <option value="PUT">PUT</option>
              <option value="PATCH">PATCH</option>
              <option value="DELETE">DELETE</option>
              <option value="login">Sign in</option>
            </Select>
            <Select value={statusF} onChange={(e) => setStatusF(e.target.value)} aria-label="Filter by status">
              <option value="all">All status</option>
              <option value="ok">Success (2xx)</option>
              <option value="client">Client error (4xx)</option>
              <option value="server">Server error (5xx)</option>
            </Select>
            <Select value={rangeF} onChange={(e) => setRangeF(e.target.value)} aria-label="Time range">
              <option value="all">All time</option>
              <option value="24h">Last 24h</option>
              <option value="7d">Last 7 days</option>
              <option value="30d">Last 30 days</option>
            </Select>
            <TableSpacer />
            <ResultCount shown={list.length} noun="entries" />
            <Button variant="outline" onClick={exportCsv} disabled={list.length === 0}>
              <DownloadIcon width={15} height={15} /> Export
            </Button>
          </TableToolbar>

          {error && <p className="form-error audit-error">{error}</p>}

          <DataTable
            rows={list}
            columns={COLUMNS}
            rowKey={(r) => r.id}
            onReachEnd={exhausted ? undefined : loadMore}
            loading={loading}
            empty={
              rows.length === 0 ? 'No audit entries yet' : 'No matching entries — adjust the filters or time range.'
            }
          />
        </>
      )}
    </div>
  );
}
