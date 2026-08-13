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

import type { TFunction } from 'i18next';
import { decodeSet, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { decodeCondition } from '../lib/filterCondition';
import { isFiltered as isFilteredAgainst, sinceIso, unset } from '../lib/filterQuery';
import {
  AUDIT_ACTIONS,
  AUDIT_STATUS_CLASSES,
  type AuditAction,
  type AuditQuery,
  type AuditRow,
  type AuditStatusClass,
} from '../types/api';

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

/**
 * The Access ▸ Audit filter row, keyed by `Column.key` (ADR-053 Inc.4).
 *
 * The keys are the API's own parameter names (`q` / `action` / `status` / `range`) rather than the
 * old column names (`user`, `action`, `status`, `time`), so `filtersFromState` is a rename-free
 * mapping. This screen keeps its filters in component state, not the URL, so no bookmark depends on
 * them — unlike History, where the same choice was forced by saved links.
 *
 * ⚠️ **`q` is a two-column search the filter row cannot express honestly.** The endpoint matches it
 * against the username **and** the action, so it is mounted on the User column, which is the more
 * useful half and the one an operator reaches for — but typing a path fragment there will also
 * match. Narrowing the API to a `username` parameter (and giving the action column its own text
 * condition) is a backend increment; until then this is the imprecision, stated rather than hidden.
 *
 * The enums are `single` for the same reason History's are: `GET /api/v1/audit` takes one `action`
 * and one `status`, and a multi-select over them would send one of the ticked values.
 */
export function auditFilters(t: TFunction): Record<string, ColumnFilterSpec<AuditRow>> {
  return {
    q: {
      kind: 'text',
      // Contains only: the endpoint does a case-insensitive substring, and there is no regex
      // parameter to carry a pattern to it. Offering the toggle would promise what cannot be sent.
      modes: ['contains'],
      readText: (r) => [r.username, r.action],
      containsSemantics: 'substring',
      placeholder: t('audit.cols.user'),
    },
    action: {
      kind: 'enum',
      single: true,
      options: AUDIT_ACTIONS.map((a) => ({ value: a, label: t(`audit.action.${a}`) })),
      readValue: (r) => r.action,
      allLabel: t('audit.filter.allActions'),
    },
    status: {
      kind: 'enum',
      single: true,
      options: AUDIT_STATUS_CLASSES.map((s) => ({ value: s, label: t(`audit.status.${s}`) })),
      // Server-side: the class is derived in SQL from the numeric status, never read here.
      readValue: (r) => String(r.status),
      allLabel: t('audit.filter.allStatuses'),
    },
    range: {
      kind: 'range',
      presets: AUDIT_RANGES.map((r) => ({
        value: r,
        label: t(`audit.range.${r}`),
        seconds: null,
      })),
      defaultPreset: DEFAULT_FILTERS.range,
    },
  };
}

/** The flat row state ⟷ the request shape. The URL codec stays this module's existing one; the
 *  filter row does not get a second one writing the same params (one handler, one write). */
export function stateFromFilters(f: AuditFilters): FilterState {
  return { q: f.q, action: f.action, status: f.status, range: f.range };
}

export function filtersFromState(s: FilterState): AuditFilters {
  const one = (v: string | undefined) => decodeSet(v ?? '')[0] ?? '';
  const range = s.range ?? '';
  return {
    // The text column stores an encoded condition; only its term reaches this API.
    q: decodeCondition(s.q ?? '').term,
    action: one(s.action) as AuditFilters['action'],
    status: one(s.status) as AuditFilters['status'],
    range: (AUDIT_RANGES as readonly string[]).includes(range)
      ? (range as AuditRange)
      : DEFAULT_FILTERS.range,
  };
}
