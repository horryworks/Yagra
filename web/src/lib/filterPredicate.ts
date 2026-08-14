// SPDX-License-Identifier: AGPL-3.0-only
// The client-side row predicate for the column filter row (ADR-053 decision 4).
//
// This is the half of the filtering that DOES generalize. `filterQuery.ts` opens by refusing a
// `makeFilterModule<T>()` factory, and that refusal stands for the server-side `queryFor` — those
// differ by their whole field set, not by a value, and the multi-value spellings differ per
// endpoint on top of that. But the ~20 hand-written `matchesX(row, filters)` functions differ by
// exactly one thing: which strings a column reads off a row. That is `valueOf`, and it is the same
// argument `lib/tableSort.ts::SortValues` already won. So: the query stays hand-written, the
// predicate becomes one function. `filterQuery.ts`'s header now says which half is which.

import {
  CUSTOM_RANGE,
  decodeNumberRange,
  decodeRange,
  decodeSet,
  rangeSecondsOf,
  type FilterState,
  type FilterableColumn,
  type ColumnFilterSpec,
} from './columnFilter';
import { compileCondition, decodeCondition } from './filterCondition';

/** One column's compiled test, or `null` when that column is not narrowing. */
function compileColumn<T>(
  spec: ColumnFilterSpec<T>,
  raw: string,
  nowMs: number,
): ((row: T) => boolean) | null {
  if (spec.kind === 'enum') {
    if (!spec.readValue) return null; // server-side: the set is already in the query
    const read = spec.readValue;
    const want = new Set(decodeSet(raw));
    if (want.size === 0) return null;
    return (row) => {
      const v = read(row);
      if (v == null) return false;
      return typeof v === 'string' ? want.has(v) : v.some((x) => want.has(x));
    };
  }
  if (spec.kind === 'text') {
    if (!spec.readText) return null; // server-side: the term is already in the query
    const read = spec.readText;
    const test = compileCondition(decodeCondition(raw));
    return test === null ? null : (row) => test(read(row));
  }
  if (spec.kind === 'values') {
    if (!spec.readValues) return null; // server-side: the set is already in the query
    const read = spec.readValues;
    const want = new Set(decodeSet(raw));
    if (want.size === 0) return null;
    return (row) => {
      const v = read(row);
      if (v == null) return false;
      return typeof v === 'string' ? want.has(v) : v.some((x) => want.has(x));
    };
  }
  if (spec.kind === 'number') {
    if (!spec.readNumber) return null; // server-side: the bounds are already in the query
    const read = spec.readNumber;
    const { min, max } = decodeNumberRange(raw);
    if (min === null && max === null) return null;
    return (row) => {
      const n = read(row);
      // A row with no number is not "0" and not "matches everything" — it is a row the question
      // cannot be asked of, so a bound excludes it. Same rule the range kind applies to a null time.
      if (n == null || !Number.isFinite(n)) return false;
      return (min === null || n >= min) && (max === null || n <= max);
    };
  }
  // range
  if (!spec.readTime) return null; // server-side: the window is already in the query, not here
  const at = spec.readTime;
  const value = decodeRange(raw, spec);
  if (value.preset === CUSTOM_RANGE) {
    // A bare `datetime-local` string has no zone, which JS reads as local time — the same reading
    // the input itself uses, so the window means what the operator saw when they typed it.
    const from = value.from ? Date.parse(value.from) : null;
    const to = value.to ? Date.parse(value.to) : null;
    if (from === null && to === null) return null;
    return (row) => {
      const ms = at(row);
      if (ms == null) return false;
      return (from === null || ms >= from) && (to === null || ms <= to);
    };
  }
  // Shared with every server-side `queryFor`, which asks the same question of the same spec — the
  // browser resolves the window to a floor, the query resolves it to a `since`.
  const secs = rangeSecondsOf(spec, raw);
  if (secs === null) return null; // "all time", or a preset a stale URL named that no longer exists
  const floor = nowMs - secs * 1000;
  return (row) => {
    const ms = at(row);
    return ms != null && ms >= floor;
  };
}

/**
 * Build the row predicate once for a given filter state.
 *
 * Prefer this over calling `matchesFilters` in a loop: a regex is compiled here, once, instead of
 * once per row. `nowMs` is a parameter rather than a `Date.now()` call so a relative range is
 * testable without faking the clock — the shape `filterQuery.ts::sinceIso` already uses.
 */
export function buildPredicate<T>(
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
  nowMs: number,
): (row: T) => boolean {
  const tests = columns
    .map((c) => compileColumn(c.filter, state[c.key] ?? '', nowMs))
    .filter((t): t is (row: T) => boolean => t !== null);
  if (tests.length === 0) return () => true;
  return (row) => tests.every((t) => t(row));
}

/** Whether one row passes every column filter. Convenience over `buildPredicate` for tests and for
 *  the odd single-row check; do not call it per row over a list. */
export function matchesFilters<T>(
  row: T,
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
  nowMs: number,
): boolean {
  return buildPredicate(columns, state, nowMs)(row);
}

/** Apply every column filter to a list. */
export function applyFilters<T>(
  rows: readonly T[],
  columns: readonly FilterableColumn<T>[],
  state: FilterState,
  nowMs: number,
): T[] {
  return rows.filter(buildPredicate(columns, state, nowMs));
}
