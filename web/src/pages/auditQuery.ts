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
import {
  normalizeSets,
  rangeSecondsIn,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { decodeCondition } from '../lib/filterCondition';
import { sinceIso, unset } from '../lib/filterQuery';
import { rangePresets, type RangeToken } from '../lib/filterPresets';
import {
  AUDIT_ACTIONS,
  AUDIT_STATUS_CLASSES,
  type AuditQuery,
  type AuditRow,
} from '../types/api';

/**
 * Rows per request. Matches the backend's default and stays under its 500 ceiling, so a page is one
 * round trip and "a short page" is a reliable end-of-results signal.
 */
export const PAGE_SIZE = 100;

/** The time windows the screen offers. The lengths are `filterPresets.ts`'s (ADR-053 Inc.10);
 *  `satisfies` is what makes a token added here without one there a compile error. */
export const AUDIT_RANGES = ['24h', '7d', '30d', 'all'] as const satisfies readonly RangeToken[];

/** The columns this module's functions read. Every one of them takes the screen's
 *  [`FilterState`] plus the columns it was drawn from — there is no second, API-named copy of the
 *  state to keep in step (ADR-053 Inc.10, 決定 AA). */
export type AuditColumns = readonly FilterableColumn<AuditRow>[];

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query string,
 * whereas `action=` would reach the backend as an empty string and be rejected as an unknown action
 * — a filter nobody set turning into a 400.
 *
 * ⚠️ **`normalizeSets` is not optional here.** The state comes straight off the controls or off a
 * hand-typed query string, and `?action=bogus` would otherwise be forwarded verbatim to an endpoint
 * that rejects unknown actions. Dropping it is the browser's job, exactly as keeping it is the API's.
 *
 * `nowMs` is a parameter so a relative range is testable without faking the clock.
 */
export function queryFor(
  columns: AuditColumns,
  s: FilterState,
  before: string | null,
  nowMs: number,
): AuditQuery {
  const f = normalizeSets(columns, s);
  return {
    // The text column stores an encoded condition (`filterCondition.ts`); only its term reaches
    // this API, which has neither a regex nor a negated form for `q`.
    q: unset(decodeCondition(f.q ?? '').term),
    action: unset(f.action),
    status: unset(f.status),
    since: sinceIso(rangeSecondsIn(columns, f), nowMs),
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
export function exportUrl(columns: AuditColumns, s: FilterState, nowMs: number): string {
  // Built from `queryFor` rather than beside it: the two must carry the same filter, and the way
  // that stops being true is one of them gaining a column the other does not know about.
  const q = queryFor(columns, s, null, nowMs);
  const params = new URLSearchParams();
  const add = (k: string, v: string | undefined) => {
    if (v) params.set(k, v);
  };
  add('q', q.q);
  add('action', q.action);
  add('status', q.status);
  add('since', q.since);
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

/**
 * The Access ▸ Audit filter row, keyed by `Column.key` (ADR-053 Inc.4).
 *
 * The keys are the API's own parameter names (`q` / `action` / `status` / `range`) rather than the
 * old column names (`user`, `action`, `status`, `time`), which is why [`queryFor`] can read the
 * state directly instead of mapping it through a second, API-named object. This screen keeps its
 * filters in component state, not the URL, so no bookmark depends on them — unlike History, where
 * the same choice was forced by saved links.
 *
 * ⚠️ **`q` is a two-column search the filter row cannot express honestly.** The endpoint matches it
 * against the username **and** the action, so it is mounted on the User column, which is the more
 * useful half and the one an operator reaches for — but typing a path fragment there will also
 * match. Narrowing the API to a `username` parameter (and giving the action column its own text
 * condition) is a backend increment; until then this is the imprecision, stated rather than hidden.
 *
 * Both enums are multi-select: since ADR-053 Inc.4b `GET /api/v1/audit` takes `action` and `status`
 * as comma-separated sets. `status` is worth a note — a class is a *range* of HTTP statuses, so a
 * set of classes is a set of ranges, and the backend walks them with `unnest` rather than an `IN`.
 * Nothing about that is visible here, which is the point.
 *
 * ⚠️ **Declared in the table's column order.** The page derives its filter list from this record
 * rather than from the columns (so that a resolved entity name cannot churn the list's identity and
 * refetch the log), and the mobile sheet lists filters in the order it is given.
 */
export function auditFilters(t: TFunction): Record<string, ColumnFilterSpec<AuditRow>> {
  return {
    // ⚠️ **No column here carries a row accessor** (ADR-053 Inc.10), and `status` is why the rule
    // is worth stating rather than assuming. It used to read `String(r.status)` — `"404"` — while
    // its options are status *classes* (`4xx`). Every token would have failed to match, so the
    // first browser-side pass anyone wired onto this screen would have emptied the table under a
    // filter that looked ordinary. The class is derived in SQL; there is nothing on the row to read.
    range: {
      kind: 'range',
      // ⚠️ The presets carry their **real** lengths now (Inc.10), where they used to carry
      // `seconds: null` "because this list is server-side". That null was inert — the predicate
      // returns at the missing `readTime` above it — and its only effect was to force a private
      // seconds table into this file for `queryFor` to read.
      presets: rangePresets(AUDIT_RANGES, t, 'audit.range.'),
      // `all` rather than a bounded window, deliberately, and it is the one place this screen
      // differs from All findings. `audit_log_at_idx` orders the table by the same column the
      // cursor pages on, so an unfiltered first page is an index scan of 100 rows however large the
      // log is — the cost `findings` avoids does not exist here. And an audit log that silently
      // withheld older entries on open would be the same class of bug the server-side move fixed.
      //
      // ⚠️ This literal is the screen's whole default state now (Inc.10): `defaultFilters(columns)`
      // derives the rest, so there is no hand-written defaults object to forget a key in.
      defaultPreset: 'all',
    },
    q: {
      kind: 'text',
      // Contains only: the endpoint does a case-insensitive substring, and there is no regex
      // parameter to carry a pattern to it. Offering the toggle would promise what cannot be sent.
      modes: ['contains'],
      containsSemantics: 'substring',
      placeholder: t('audit.cols.user'),
    },
    action: {
      kind: 'enum',
      options: AUDIT_ACTIONS.map((a) => ({ value: a, label: t(`audit.action.${a}`) })),
      allLabel: t('audit.filter.allActions'),
    },
    status: {
      kind: 'enum',
      // `statusClass`, not `status` — that is the prefix the locales carry and the one
      // `i18nEnumKeys.test.ts` pins. A key absent from BOTH locales passes the parity gate, so
      // this read as translated right up until the dropdown drew `audit.status.ok`.
      options: AUDIT_STATUS_CLASSES.map((s) => ({ value: s, label: t(`audit.statusClass.${s}`) })),
      allLabel: t('audit.filter.allStatuses'),
    },
  };
}
