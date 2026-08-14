// SPDX-License-Identifier: AGPL-3.0-only
// Facet counts for a multi-select column filter (ADR-053 decision 6).
//
// **The counting rule is the whole content of this module, and getting it wrong is what makes a
// filter unreadable rather than merely wrong.** A facet's counts are computed over the rows that
// pass every OTHER column's filter — never over the rows currently displayed. Count the displayed
// rows instead and selecting `syslog` immediately shows `trap: 0`, so the operator is told the
// thing they might switch to has nothing in it, when in fact it has thousands. Excel has always
// done it this way; the version that reads correctly is the version that excludes the column's own
// filter.
//
// The consequence for the server-side lists is decision 6's other half: because each facet needs a
// *different* query (its own filter dropped), the counts cost one request per popover. They are
// therefore fetched **when the popover opens**, never on load. ADR-023 puts UI load third behind
// polling and alert evaluation; firing five aggregate queries per page load to decorate checkboxes
// nobody clicked is the shape that starves the poller.

import { type FilterState, type FilterableColumn } from './columnFilter';
import { buildPredicate } from './filterPredicate';

/** The rows a column's facet counts are computed over: everything passing the *other* filters. */
export function rowsExcluding<T>(
  rows: readonly T[],
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
  exceptKey: string,
  nowMs: number,
): T[] {
  const others = columns.filter((c) => c.key !== exceptKey);
  return rows.filter(buildPredicate(others, state, nowMs));
}

/**
 * Count each option of one enum column, over the rows that pass every other filter.
 *
 * Every option in the spec gets a key, including the ones that count zero — a checkbox that
 * vanishes when its count reaches zero cannot be un-selected, and an option list whose length
 * changes as you click it is unusable.
 */
export function facetCounts<T>(
  rows: readonly T[],
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
  key: string,
  nowMs: number,
): Record<string, number> {
  const col = columns.find((c) => c.key === key);
  if (!col || col.filter.kind !== 'enum') return {};
  const spec = col.filter;
  // No accessor means the column is answered by the server, so there are no rows here to count and
  // no honest number to return. `{}` is what the cell already renders as "no counts offered" —
  // better than zeros, which would read as "nothing matches any of these".
  const read = spec.readValue;
  if (!read) return {};
  const counts: Record<string, number> = Object.fromEntries(spec.options.map((o) => [o.value, 0]));
  for (const row of rowsExcluding(rows, columns, state, key, nowMs)) {
    const v = read(row);
    if (v == null) continue;
    for (const one of typeof v === 'string' ? [v] : v) {
      // A value the spec does not list is a row the operator cannot select for. Counting it into a
      // key nobody renders would make the totals not add up; dropping it is the honest choice, and
      // it is why the option list — not the data — is the source of the keys.
      if (one in counts) counts[one] += 1;
    }
  }
  return counts;
}
