// SPDX-License-Identifier: AGPL-3.0-only
// Filter-row state backed by the query string (ADR-053).
//
// A thin wrapper over `readFilterParams` / `writeFilterParams` — the codec is pure and tested
// there, and this adds only the two things a hook can add: the router binding, and the clock.
//
// **The clock is the part worth reading.** A relative range resolves to an absolute lower bound,
// and `eventRange.ts::boundsFor` requires that bound to be resolved **once, when the range is
// chosen** — not per request. One recomputed on every render creeps forward between "load older"
// pages and silently drops rows the keyset cursor was walking towards. So `nowMs` is pinned in a ref
// and re-read only when the range column's own value changes.

import { useCallback, useMemo, useRef } from 'react';
import { useSearchParams } from 'react-router-dom';
import {
  readFilterParams,
  writeFilterParams,
  type FilterState,
  type FilterableColumn,
} from './columnFilter';

export interface FilterParams {
  filters: FilterState;
  setFilters: (next: FilterState) => void;
  /** The instant relative ranges resolve against — stable until the range itself changes. */
  nowMs: number;
}

export function useFilterParams<T>(columns: readonly FilterableColumn<T>[]): FilterParams {
  const [searchParams, setSearchParams] = useSearchParams();

  const filters = useMemo(() => readFilterParams(columns, searchParams), [columns, searchParams]);

  const setFilters = useCallback(
    (next: FilterState) => {
      const params = new URLSearchParams(searchParams);
      writeFilterParams(columns, params, next);
      // `replace` so a settled keystroke does not push a history entry and make Back mean "the
      // previous character" instead of "the previous screen" (`filterParams.ts`).
      setSearchParams(params, { replace: true });
    },
    [columns, searchParams, setSearchParams],
  );

  const rangeKey = columns.find((c) => c.filter.kind === 'range')?.key;
  const rangeValue = rangeKey ? (filters[rangeKey] ?? '') : '';
  const pinned = useRef<{ value: string; ms: number } | null>(null);
  if (pinned.current === null || pinned.current.value !== rangeValue) {
    pinned.current = { value: rangeValue, ms: Date.now() };
  }

  return { filters, setFilters, nowMs: pinned.current.ms };
}
