// SPDX-License-Identifier: AGPL-3.0-only
// A re-read list keeps the order the operator is looking at (ADR-216). The server orders some
// lists by a value the operator's own action changes — Missing IP prefixes by open gaps, the two
// schedule lists by the next run — so drawing each new answer in its own order moved the row just
// acted on somewhere else, often off screen. The answer's contents are taken; its order is not,
// until the operator does something that asks for a new order (the `reset` value changes).

import { useState } from 'react';

/** `rows` in the order `previous` names. A row `previous` does not name goes after the ones it
 *  does, in `rows`' own order; a name `previous` holds that `rows` no longer has is dropped.
 *  Every row returned is the one in `rows` — only the order comes from `previous`. */
export function keepOrder<T>(
  previous: readonly string[],
  rows: readonly T[],
  key: (row: T) => string,
): T[] {
  const place = new Map(previous.map((k, i) => [k, i]));
  const known: { row: T; at: number }[] = [];
  const fresh: T[] = [];
  for (const row of rows) {
    const at = place.get(key(row));
    if (at === undefined) fresh.push(row);
    else known.push({ row, at });
  }
  known.sort((a, b) => a.at - b.at);
  return [...known.map((k) => k.row), ...fresh];
}

/** `rows`, kept in the order they were first drawn in until `reset` changes. The first answer, and
 *  the first answer after `reset` changes, are drawn in the server's order. `key` is read on every
 *  answer, so it need not be stable. */
export function useStableOrder<T>(rows: readonly T[], key: (row: T) => string, reset: string): T[] {
  const [seen, setSeen] = useState<{ rows: readonly T[]; reset: string; out: T[] }>(() => ({
    rows,
    reset,
    out: [...rows],
  }));
  if (seen.rows === rows && seen.reset === reset) return seen.out;
  // Adjusting state while rendering, the documented way to derive it from a changed input: React
  // re-renders at once with the new value and never paints the stale one.
  const out = seen.reset === reset ? keepOrder(seen.out.map(key), rows, key) : [...rows];
  setSeen({ rows, reset, out });
  return out;
}
