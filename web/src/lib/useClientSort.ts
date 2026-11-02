// SPDX-License-Identifier: AGPL-3.0-only
// Sorting for a list held entirely in the browser, by any column that can say what a row is worth
// (ADR-228 Inc.2). The values come from `clientSort.ts`; the state lives in the URL beside the
// filters, under the same prefix (ADR-153), so a reload keeps it.
//
// It opens in the order the list arrived in (`ARRIVAL_ORDER`) and sorts from the first click on.

import { useMemo } from 'react';
import { ARRIVAL_ORDER, clientSortValues, type ClientSortColumn } from './clientSort';
import { sortRows, type SortState } from './tableSort';
import { useSortParams } from './useSortParams';

export interface ClientSort<T, C> {
  /** `rows` in the chosen order — the arrival order until a header is pressed. */
  rows: T[];
  /** The columns with `sortable` set on every one that can be sorted. Hand these to `DataTable`. */
  columns: (C & { sortable?: boolean })[];
  sort: SortState;
  setSort: (next: SortState) => void;
}

export function useClientSort<T, C extends ClientSortColumn<T>>(
  columns: readonly C[],
  rows: readonly T[],
  opts?: { prefix?: string },
): ClientSort<T, C> {
  const values = useMemo(() => clientSortValues(columns), [columns]);
  const keys = useMemo(() => Object.keys(values), [values]);
  const [sort, setSort] = useSortParams(keys, ARRIVAL_ORDER, opts?.prefix ?? '');
  const sorted = useMemo(
    () => (sort.by === '' ? [...rows] : sortRows(rows, sort, values)),
    [rows, sort, values],
  );
  const sortable = useMemo(
    () => columns.map((c) => (c.key in values ? { ...c, sortable: true } : c)),
    [columns, values],
  );
  return { rows: sorted, columns: sortable, sort, setSort };
}
