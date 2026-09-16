// SPDX-License-Identifier: AGPL-3.0-only
// A table's sort held in the URL as `?sort=&dir=` (ADR-153).
//
// The router binding over `readSortParams` / `writeSortParams` in `tableSort.ts`, where the rules
// live and are tested: an unknown column reads as the default, and the default is no keys at all.
// `replace`, because re-sorting a table is not a place Back should step through.

import { useCallback } from 'react';
import { useSearchParams } from 'react-router-dom';
import { readSortParams, writeSortParams, type SortState } from './tableSort';

export function useSortParams(
  sortable: readonly string[],
  fallback: SortState,
): [SortState, (next: SortState) => void] {
  const [params, setParams] = useSearchParams();
  const sort = readSortParams(params, sortable, fallback);
  const set = useCallback(
    (next: SortState) => {
      const p = new URLSearchParams(params);
      writeSortParams(p, next, fallback);
      setParams(p, { replace: true });
    },
    [params, setParams, fallback],
  );
  return [sort, set];
}
