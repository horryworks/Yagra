// SPDX-License-Identifier: AGPL-3.0-only
// What each column of a browser-held list sorts on (ADR-228 Inc.2).
//
// A list whose every row is in the browser may be sorted there (`tableSort.ts`, the first of its two
// right answers). Asking every screen to write a sort value per column would be the same fact twice:
// a filterable column already says how to read its value out of a row, for its filter. So the value
// is derived from that reader, and a column states one of its own (`sortValue`) only when it has no
// filter, or when what it filters on is not what it should order by.
//
// How each kind orders:
//   - enum   — by the option's position in the spec, so a severity sorts critical → info rather than
//              alphabetically. A value the options do not list sorts after the ones they do.
//   - text   — by the first string the filter reads (the one the cell shows first).
//   - range  — by the instant.
//   - number — by the number.
//   - values — by the first value, as a number when it is one ("10" after "9").
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import type { ColumnFilterSpec } from './columnFilter';
import type { SortState, SortValues } from './tableSort';

type SortValue = string | number | null | undefined;

/** The part of a column this needs: its key, its filter spec, and an optional value of its own. */
export interface ClientSortColumn<T> {
  key: string;
  filter?: ColumnFilterSpec<T>;
  /** What the column orders by when its filter does not say, or says something else. */
  sortValue?: (row: T) => SortValue;
}

/** The state a list opens in: the order it arrived in, with no column marked. A click moves it to a
 *  real sort, which then alternates between the two directions (`nextSort`). */
export const ARRIVAL_ORDER: SortState = { by: '', dir: 'asc' };

const first = <V>(v: V | readonly V[] | null | undefined): V | undefined =>
  Array.isArray(v) ? (v as readonly V[])[0] : ((v ?? undefined) as V | undefined);

/** The sort value a filter spec implies, or `undefined` when its reader is absent (a server-side
 *  spec reads nothing in the browser). */
export function specSortValue<T>(spec: ColumnFilterSpec<T>): ((row: T) => SortValue) | undefined {
  switch (spec.kind) {
    case 'enum': {
      const read = spec.readValue;
      if (!read) return undefined;
      const order = new Map(spec.options.map((o, i) => [o.value, i]));
      return (row) => {
        const v = first(read(row));
        if (v === undefined || v === null || v === '') return null;
        return order.get(v) ?? spec.options.length;
      };
    }
    case 'text': {
      const read = spec.readText;
      if (!read) return undefined;
      return (row) => read(row).find((s) => s !== null && s !== undefined && s !== '') ?? null;
    }
    case 'range':
      return spec.readTime;
    case 'number':
      return spec.readNumber;
    case 'values': {
      const read = spec.readValues;
      if (!read) return undefined;
      return (row) => {
        const v = first(read(row));
        if (v === undefined || v === null || v === '') return null;
        const n = Number(v);
        return Number.isFinite(n) ? n : v;
      };
    }
  }
}

/** One value per sortable column, keyed by column key. A column with neither a `sortValue` nor a
 *  filter that reads in the browser is left out, and so is not offered as sortable. */
export function clientSortValues<T>(columns: readonly ClientSortColumn<T>[]): SortValues<T> {
  const out: SortValues<T> = {};
  for (const c of columns) {
    const value = c.sortValue ?? (c.filter ? specSortValue(c.filter) : undefined);
    if (value) out[c.key] = value;
  }
  return out;
}
