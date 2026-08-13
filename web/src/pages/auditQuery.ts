// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Audit — the query logic, as pure functions.
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.
//
// These filters used to run in the browser over the already-loaded pages, which made the toolbar
// lie: "last 30 days, DELETE only" examined the newest 100 rows and silently hid every older match,
// and Export handed the operator that same partial set. In a log whose purpose is completeness that
// is a correctness bug, not a missing feature. The controls are unchanged; only where they apply is.

import { isFiltered as isFilteredAgainst, sinceIso, unset } from '../lib/filterQuery';
import type { AuditAction, AuditQuery, AuditStatusClass } from '../types/api';

/**
 * Rows per request. Matches the backend's default and stays under its 500 ceiling, so a page is one
 * round trip and "a short page" is a reliable end-of-results signal.
 */
export const PAGE_SIZE = 100;

/** The time windows the screen offers. */
export const AUDIT_RANGES = ['24h', '7d', '30d', 'all'] as const;
export type AuditRange = (typeof AUDIT_RANGES)[number];

const RANGE_SECS: Record<AuditRange, number | null> = {
  '24h': 86_400,
  '7d': 7 * 86_400,
  '30d': 30 * 86_400,
  all: null,
};

/** The screen's filter state. `''` means "no filter" for each optional field. */
export interface AuditFilters {
  q: string;
  action: AuditAction | '';
  status: AuditStatusClass | '';
  range: AuditRange;
}

/**
 * The default view: everything, unfiltered.
 *
 * `all` rather than a bounded window, deliberately, and it is the one place this screen differs from
 * Saved findings. `audit_log_at_idx` orders the table by the same column the cursor pages on, so an
 * unfiltered first page is an index scan of 100 rows however large the log is — the cost `findings`
 * avoids does not exist here. And an audit log that silently withheld older entries on open would be
 * the same class of bug this change exists to fix.
 */
export const DEFAULT_FILTERS: AuditFilters = { q: '', action: '', status: '', range: 'all' };

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query string,
 * whereas `action=` would reach the backend as an empty string and be rejected as an unknown action
 * — a filter nobody set turning into a 400.
 *
 * `nowMs` is a parameter so a relative range is testable without faking the clock.
 */
export function queryFor(f: AuditFilters, before: string | null, nowMs: number): AuditQuery {
  return {
    q: unset(f.q),
    action: unset(f.action),
    status: unset(f.status),
    since: sinceIso(RANGE_SECS[f.range], nowMs),
    before: before ?? undefined,
    limit: PAGE_SIZE,
  };
}

/**
 * The download URL for the CSV of everything matching `f`.
 *
 * Deliberately **not** `queryFor` with a bigger limit: the export endpoint takes no cursor and no
 * limit, because "the second page of an export" is not a thing an operator can act on. What it
 * shares with the list is the *filter*, so the two answer questions about the same set — the point
 * of the whole change, since the button used to write out whatever had been scrolled to.
 *
 * A URL rather than a fetch: the browser's own download handles the file, so a large export never
 * passes through JavaScript memory. The session cookie rides along; a token-authenticated client
 * calls the endpoint itself.
 */
export function exportUrl(f: AuditFilters, nowMs: number): string {
  const params = new URLSearchParams();
  const add = (k: string, v: string | undefined) => {
    if (v) params.set(k, v);
  };
  add('q', unset(f.q));
  add('action', unset(f.action));
  add('status', unset(f.status));
  add('since', sinceIso(RANGE_SECS[f.range], nowMs));
  const qs = params.toString();
  return qs ? `/api/v1/audit/export.csv?${qs}` : '/api/v1/audit/export.csv';
}

/**
 * The cursor for the page after `rows`, or `null` when there is no next page.
 *
 * A short page means the filtered query ran out of matches, not that the log did — which is exactly
 * what makes this correct only now that the filter is in SQL. While it ran in the browser, a full
 * page could still yield nothing visible, so "short page" said nothing about the end of the log.
 */
export function nextCursor(rows: readonly { at: string }[]): string | null {
  const last = rows.at(-1);
  return rows.length < PAGE_SIZE || !last ? null : last.at;
}

/**
 * Append a page to what is already held, dropping rows already present.
 *
 * `audit_log.at` defaults to `now()`, which is the *transaction* timestamp — but `AuditRepo::record`
 * writes one row per transaction, so two entries collide only if two transactions start in the same
 * microsecond. (Contrast `alert_history`, whose batch writer guarantees ties and therefore needs a
 * composite cursor.) This dedup is the cheap insurance against that residual case: a duplicate React
 * key is a silent misrender rather than a visible error.
 */
export function appendPage<T extends { id: string }>(have: readonly T[], page: readonly T[]): T[] {
  const seen = new Set(have.map((r) => r.id));
  return [...have, ...page.filter((r) => !seen.has(r.id))];
}

/** Whether anything is narrowing the log — drives the empty state's wording.
 *
 *  ⚠️ Must not be replaced by a `rows.length` check. That worked only while every row was in the
 *  browser; with the filter in SQL, a filtered query that legitimately returns zero is
 *  indistinguishable from an empty log, and the screen would say "no audit entries" while a filter
 *  is hiding them. */
export function isFiltered(f: AuditFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_FILTERS);
}
