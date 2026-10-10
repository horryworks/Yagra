// SPDX-License-Identifier: AGPL-3.0-only
// Keyset paging for the "Load more" lists (ADR-202).
//
// Four lists page the same way — audit log, notification delivery log, alert history, saved
// findings — and each held its own copy of both functions. What differs between them is which
// fields of the last row form the cursor and how many rows a page holds, so those are the
// arguments; the rule itself is written once here.

/**
 * The cursor for the page after `rows`, or `null` when there is no next page.
 *
 * A page shorter than `pageSize` is the end. That is only true because each list's filter runs in
 * the query: a full page filtered afterwards could show nothing and still have more behind it.
 * `cursorOf` reads the cursor out of the last row — when several rows can share a timestamp, it
 * must return the row id beside it, or a page boundary inside that tie skips rows.
 */
export function nextCursorFrom<R, C>(
  rows: readonly R[],
  pageSize: number,
  cursorOf: (last: R) => C,
): C | null {
  const last = rows.at(-1);
  return rows.length < pageSize || last === undefined ? null : cursorOf(last);
}

/**
 * Append a page to what is already held, dropping rows already present.
 *
 * Defensive rather than expected: every list's cursor is total. But a duplicate React `key` is a
 * silent misrender rather than a visible error, so the cost of "impossible" being wrong is high.
 */
export function appendPage<T extends { id: string | number }>(
  have: readonly T[],
  page: readonly T[],
): T[] {
  const seen = new Set(have.map((r) => r.id));
  return [...have, ...page.filter((r) => !seen.has(r.id))];
}
