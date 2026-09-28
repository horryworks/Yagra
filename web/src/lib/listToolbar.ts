// SPDX-License-Identifier: AGPL-3.0-only
// The judgement half of a list's toolbar (ADR-184 increment 28). `components/ui/ListToolbar.tsx`
// is the layout half; everything here is pure so a node-environment test reaches it.
//
// Thirty-five toolbars wrote the same row by hand: the filter button, "clear all filters", a
// spacer, the result count, and — after the table, far from the button that opened it — the
// mobile filter sheet with a hand-built label map. Written 35 times, the copies had drifted on
// each of the small decisions this file now makes once: which word the count's noun agrees with,
// whether "N of M" shows while nothing is narrowing the list, and what the sheet calls a column.
import {
  defaultFilters,
  isAnyFiltered,
  type FilterState,
  type FilterableColumn,
} from './columnFilter';
import type { FilterParams } from './useFilterParams';

/** What a toolbar needs to know about a list's filters. `useClientFilters`' result is one as it
 *  stands; a server-filtered list builds one with `serverToolbarFilters`. */
export interface ToolbarFilters<T> {
  filterCols: readonly FilterableColumn<T>[];
  filters: FilterState;
  setFilters: (next: FilterState) => void;
  /** Reset everything, in one state write — see `ClearFilters.onClear`. */
  clear: () => void;
  counts?: Record<string, Record<string, number>>;
  anyFiltered: boolean;
  /** A control outside the filter row is also narrowing the list (a node picker). */
  extraActive?: boolean;
  /** The state that counts as "nothing set", for a table whose own default narrows. */
  baseline?: FilterState;
}

/**
 * What the sheet calls each column: the header when it is text, the key otherwise.
 *
 * The same rule `DataTable` applies to the filter row and the resize handles, from here, so the
 * phone and the desktop name a column the same way. Toolbars used to build this map by hand, three
 * ways (`String(c.header)`, a `t('cols.…')` per key, a literal object) — and `String()` of a header
 * that is an element renders `[object Object]`.
 */
export function columnLabels(
  columns: readonly { key: string; header: unknown }[],
): Record<string, string> {
  return Object.fromEntries(
    columns.map((c) => [c.key, typeof c.header === 'string' ? c.header : c.key]),
  );
}

/** The filterable columns the labels leave with nothing but their key — the columns whose header
 *  is not text. A screen with any passes the missing names itself. */
export function labelGaps<T>(
  cols: readonly FilterableColumn<T>[],
  labels: Record<string, string>,
): string[] {
  return cols.filter((c) => !labels[c.key] || labels[c.key] === c.key).map((c) => c.key);
}

/**
 * The result count's numbers.
 *
 * - **"N of M" only while something is narrowing the list** (T2). Unfiltered, the two are the same
 *   number and "12 of 12 users" says it twice; four screens said it anyway.
 * - **The noun agrees with the number beside it** (T1): "1 of 5 windows", not "1 of 5 window".
 *   Most screens pluralized by `shown`, which is the number *before* "of", so English read wrong
 *   whenever exactly one row matched.
 */
export function resultCount(i: { shown: number; total?: number; anyFiltered: boolean }): {
  shown: number;
  total: number | undefined;
  nounCount: number;
} {
  const total = i.anyFiltered ? i.total : undefined;
  return { shown: i.shown, total, nounCount: total ?? i.shown };
}

/**
 * `ToolbarFilters` for a list filtered by the server (`useFilterParams`).
 *
 * `extra` is a narrowing control outside the filter row. Its reset is folded into the **same** URL
 * write as the columns' (`also`): two writes in one handler are both built from one render's
 * snapshot and the second restores what the first cleared — "clear all filters" on the Events page
 * once did nothing at all for exactly that reason. `onClear` is for state the control holds outside
 * the URL (a picker's label).
 */
export function serverToolbarFilters<T>(
  filterCols: readonly FilterableColumn<T>[],
  params: Pick<FilterParams, 'filters' | 'setFilters'>,
  extra?: { active: boolean; clear: (p: URLSearchParams) => void; onClear?: () => void },
  counts?: Record<string, Record<string, number>>,
): ToolbarFilters<T> {
  return {
    filterCols,
    filters: params.filters,
    setFilters: (next) => params.setFilters(next),
    clear: () => {
      extra?.onClear?.();
      params.setFilters(defaultFilters(filterCols), extra?.clear);
    },
    counts,
    anyFiltered: isAnyFiltered(filterCols, params.filters) || !!extra?.active,
    extraActive: extra?.active,
  };
}
