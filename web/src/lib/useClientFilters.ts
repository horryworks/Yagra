// SPDX-License-Identifier: AGPL-3.0-only
// The filter row on a list that is already in the browser (ADR-053 Inc.3).
//
// Every client-side screen needs the same five things — the filterable columns, the state, the
// predicate applied, the facet counts, and whether anything is narrowing — and Inc.3 converts eight
// of them at once. Written per screen that is eight copies of the memo dependencies, and the way
// those copies go wrong is not a crash: a missing `filters` in a dependency array leaves the table
// showing the previous filter's rows, which reads as the control being slow rather than broken.
//
// What this does NOT do is decide what a column reads off a row. That stays in each screen's spec
// module, and it is the thing `filterQuery.ts` has always argued is genuinely per-screen.

import { useMemo } from 'react';
import {
  defaultFilters,
  filterableColumns,
  isAnyFiltered,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from './columnFilter';
import { applyFilters } from './filterPredicate';
import { facetCounts } from './filterCounts';
import { useFilterParams } from './useFilterParams';

export interface ClientFilters<T> {
  /** The columns that carry a filter, in column order. Pass to `ClearFilters` / `MobileFilterSheet`. */
  filterCols: FilterableColumn<T>[];
  filters: FilterState;
  setFilters: (next: FilterState) => void;
  /** Reset every column. One state write — see `useFilterParams.setFilters` for why that matters. */
  clear: () => void;
  /** `rows` with every column filter applied. Sorting stays with the caller. */
  shown: T[];
  /** Per-column option counts, excluding each column's own filter (`lib/filterCounts.ts`). */
  counts: Record<string, Record<string, number>>;
  anyFiltered: boolean;
  /** The instant relative ranges resolve against. Stable until the range changes. */
  nowMs: number;
}

/**
 * Filter state + predicate + counts for a list held entirely in the browser.
 *
 * **The state lives in the URL, always** (ADR-153). It used to be a per-screen `url` flag, off by
 * default, because the column key *is* the URL key and two tables on one route would filter each
 * other — so the screens with two or three tables kept their filters in component state, and a
 * reload threw them away. A route with more than one table now gives each table after the first a
 * `prefix` (`{ prefix: 'channels.' }` ⇒ `?channels.status=`), and whether the keys on a route are
 * disjoint is checked by the route ledger in `filterSpecRegistry.test.ts`.
 */
export function useClientFilters<T>(
  columns: readonly { key: string; filter?: ColumnFilterSpec<T> }[],
  rows: readonly T[],
  opts?: { prefix?: string },
): ClientFilters<T> {
  const filterCols = useMemo(() => filterableColumns(columns), [columns]);

  // The prefix is read out as a string before it reaches the hook: `opts` is an object literal at
  // every call site, and a new identity per render would rebuild `setFilters` on every render.
  const { filters, setFilters, nowMs } = useFilterParams(filterCols, opts?.prefix ?? '');

  const shown = useMemo(
    () => applyFilters(rows, filterCols, filters, nowMs),
    [rows, filterCols, filters, nowMs],
  );

  // Only the enum columns have options to count. A text or range column has no list to decorate,
  // and asking for one would be a pass over every row per render for nothing.
  const counts = useMemo(
    () =>
      Object.fromEntries(
        filterCols
          .filter((c) => c.filter.kind === 'enum')
          .map((c) => [c.key, facetCounts(rows, filterCols, filters, c.key, nowMs)]),
      ),
    [rows, filterCols, filters, nowMs],
  );

  return {
    filterCols,
    filters,
    setFilters,
    clear: () => setFilters(defaultFilters(filterCols)),
    shown,
    counts,
    anyFiltered: isAnyFiltered(filterCols, filters),
    nowMs,
  };
}
