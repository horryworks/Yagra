// SPDX-License-Identifier: AGPL-3.0-only
// Column sorting for `DataTable`, as pure functions.
//
// ⚠️ **`DataTable` renders the affordance and never sorts.** That is the whole design, and it is
// there to stop one specific bug: a table fed by a keyset cursor holds the pages that have been
// scrolled to, not the list. Sorting those in the browser reorders a prefix and presents it as the
// order — the same lie the Audit filters told before they moved into SQL, wearing a different
// control. A component that sorted its own `rows` would make that the default and the correct
// behaviour the special case.
//
// So the caller decides, and there are exactly two right answers:
//
//   - **Bounded by what an operator configured** (tokens, schedules, templates, channels): sort in
//     the browser with `sortRows` below. Every row is present, so the order is the real one.
//   - **Grows with the fleet** (nodes, alerts, history, events, audit, thresholds, flow): the sort
//     belongs in the query, beside the filters, and the cursor has to page on the same columns it
//     orders by (`ORDER BY` ⟷ cursor ⟷ index — `history.rs` has the test for that shape). Until an
//     endpoint offers it, those columns are simply not sortable, which is honest.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

/** Ascending or descending. There is deliberately no "unsorted" third state: a table always has
 *  some order, and cycling back to "whatever the server sent" is a state an operator cannot name. */
export type SortDir = 'asc' | 'desc';

export interface SortState {
  /** The `Column.key` being sorted on. */
  by: string;
  dir: SortDir;
}

/** The next sort state after clicking `key`.
 *
 *  Clicking the active column flips the direction; clicking a different one moves to it and starts
 *  ascending. Starting ascending rather than keeping the previous direction is the convention every
 *  spreadsheet uses, and the alternative — inheriting a `desc` from an unrelated column — reads as
 *  the click not having worked. */
export function nextSort(current: SortState, key: string): SortState {
  if (current.by !== key) return { by: key, dir: 'asc' };
  return { by: key, dir: current.dir === 'asc' ? 'desc' : 'asc' };
}

/** The URL keys a sort is held under (ADR-153). Both are in `RESERVED_URL_KEYS`, which is what stops
 *  a filter column from ever taking either. */
export const SORT_PARAM = 'sort';
export const DIR_PARAM = 'dir';

/** Read a sort out of the query string.
 *
 *  A column this table does not sort on falls back to the table's default — a stale bookmark from a
 *  build with one more sortable column opens the default order, never an order the header cannot
 *  show an arrow for. A `dir` that is not `desc` reads as `asc`, which is where a click starts. */
export function readSortParams(
  params: URLSearchParams,
  sortable: readonly string[],
  fallback: SortState,
): SortState {
  const by = params.get(SORT_PARAM);
  if (!by || !sortable.includes(by)) return fallback;
  return { by, dir: params.get(DIR_PARAM) === 'desc' ? 'desc' : 'asc' };
}

/** Write a sort into `params`, deleting both keys when it is the table's default — so a bare URL is
 *  the default order, the same rule every filter key follows. Both keys move together: a sort is one
 *  state, and `?dir=` with no `?sort=` would be a key nothing reads. */
export function writeSortParams(params: URLSearchParams, next: SortState, fallback: SortState): void {
  if (next.by === fallback.by && next.dir === fallback.dir) {
    params.delete(SORT_PARAM);
    params.delete(DIR_PARAM);
    return;
  }
  params.set(SORT_PARAM, next.by);
  params.set(DIR_PARAM, next.dir);
}

/** How one column's value is extracted for comparison, per column key.
 *
 *  A `Record` keyed by the column key rather than a `compare` on the column itself: the value a
 *  column *renders* is a `ReactNode`, and sorting on rendered output is how a "12" ends up after a
 *  "9". The caller says what the cell means. */
export type SortValues<T> = Record<string, (row: T) => string | number | null | undefined>;

/**
 * A sorted copy of `rows`.
 *
 * Never mutates: the array usually comes straight from a store or a fetch, and sorting it in place
 * would reorder something another component is rendering from.
 *
 * Rules that matter, because each is a way a table looks broken:
 *
 * - **Strings compare with `localeCompare`**, so `ä` sorts beside `a` and Japanese sorts by its own
 *   collation rather than by code point. Numeric-aware too, so `sw-2` precedes `sw-10`.
 * - **A missing value sorts last in both directions.** Reversing the sort must not fill the top of
 *   the screen with blanks — the operator flipped the direction to see the *other end* of the data,
 *   not to see the rows that have none.
 * - **The order is total.** Ties fall back to the original position, so re-sorting a stable list
 *   does not shuffle equal rows and make the table look like it refreshed.
 */
export function sortRows<T>(rows: readonly T[], sort: SortState, values: SortValues<T>): T[] {
  const value = values[sort.by];
  if (!value) return [...rows];
  const sign = sort.dir === 'asc' ? 1 : -1;
  return rows
    .map((row, index) => ({ row, index }))
    .sort((a, b) => {
      const av = value(a.row);
      const bv = value(b.row);
      const aMissing = av === null || av === undefined || av === '';
      const bMissing = bv === null || bv === undefined || bv === '';
      // Missing last regardless of direction, so flipping the sort never shows a screen of blanks.
      if (aMissing || bMissing) {
        if (aMissing && bMissing) return a.index - b.index;
        return aMissing ? 1 : -1;
      }
      let cmp: number;
      if (typeof av === 'number' && typeof bv === 'number') {
        cmp = av - bv;
      } else {
        cmp = String(av).localeCompare(String(bv), undefined, {
          numeric: true,
          sensitivity: 'base',
        });
      }
      // Stable: equal rows keep the order they arrived in.
      return cmp === 0 ? a.index - b.index : cmp * sign;
    })
    .map((x) => x.row);
}
