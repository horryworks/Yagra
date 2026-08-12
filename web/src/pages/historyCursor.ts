// SPDX-License-Identifier: AGPL-3.0-only
// Alert history — keyset paging, as pure functions.
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.

import type { AlertHistoryRow } from '../types/api';

/** Rows per request. Matches the backend default and stays under its 1000 ceiling, so one page is
 *  one round trip and a short page is the end-of-log signal. */
export const PAGE_SIZE = 100;

/**
 * The cursor for the page after `rows`, or `null` when there is no next page.
 *
 * **Both halves come from the same row, and both are required.** `recorded_at` defaults to
 * PostgreSQL's `now()`, which is the *transaction* timestamp, and the backend writes an entire
 * flush of alerts as one multi-row `INSERT` — so every row of a flush carries an identical
 * `recorded_at`. Paging on the timestamp alone meant a page boundary landing inside a flush
 * silently skipped that flush's remaining rows, and a fleet-wide event is exactly when a flush is
 * large and exactly when someone is reading this log.
 */
export function nextCursor(
  rows: readonly AlertHistoryRow[],
): { before: string; before_id: string } | null {
  const last = rows.at(-1);
  if (rows.length < PAGE_SIZE || !last) return null;
  return { before: last.recorded_at, before_id: last.id };
}

/**
 * Append a page to what is already held, dropping rows already present.
 *
 * Defensive rather than expected: the composite cursor is total, so a duplicate should be
 * impossible. But a duplicate `key` in React is a rendering bug rather than a visible one, and the
 * cost of being wrong about "impossible" here is a screen that silently misrenders — which is the
 * same class of failure this whole change exists to remove.
 */
export function appendPage(
  have: readonly AlertHistoryRow[],
  page: readonly AlertHistoryRow[],
): AlertHistoryRow[] {
  const seen = new Set(have.map((r) => r.id));
  return [...have, ...page.filter((r) => !seen.has(r.id))];
}
