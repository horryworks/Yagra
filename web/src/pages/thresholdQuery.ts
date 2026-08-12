// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Rules (Thresholds) — the filter state and the request it becomes, as pure functions.
//
// **Server-side, and it has to be.** Thresholds are the one configuration table that grows with
// the fleet (a node-level override is per node × metric), which is why the list has always been
// capped at 500 with a `truncated` flag. A filter applied in the browser would run over that prefix
// only, so "show me the cpu_util rules" would silently examine the first 500 rules and report on
// those — exactly the failure Settings ▸ Audit had, on the table that decides when the fleet pages
// someone. The predicate goes in the query; `total` comes back counting the matches.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md).

import { isFiltered as isFilteredAgainst, unset } from '../lib/filterQuery';
import { readEnumParam, readIdParam, writeEnumParam, writeIdParam } from '../lib/filterParams';
import type { Direction, ScopeLevel } from '../types/api';

/** The screen's filter state. `''` means "no filter" for each field. */
export interface ThresholdFilters {
  /** Free text over the metric name — matched as a substring, server-side. */
  q: string;
  scopeLevel: ScopeLevel | '';
  direction: Direction | '';
}

export const DEFAULT_THRESHOLD_FILTERS: ThresholdFilters = {
  q: '',
  scopeLevel: '',
  direction: '',
};

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query string,
 * whereas `scope_level=` would arrive as an empty string and be rejected as an unknown level — a
 * filter nobody set turning into a 400.
 */
export function queryFor(f: ThresholdFilters) {
  return {
    q: unset(f.q),
    scope_level: unset(f.scopeLevel),
    direction: unset(f.direction),
  };
}

/** Whether anything is narrowing the ruleset — drives the empty state's wording.
 *
 *  ⚠️ Must not be replaced by a `rows.length` check: with the predicate in SQL, a filtered query
 *  that legitimately returns zero is indistinguishable from an empty ruleset. */
export function isFiltered(f: ThresholdFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_THRESHOLD_FILTERS);
}

/** Read the filters out of the URL, so a narrowed view survives a reload and can be shared. */
export function readFilters(
  params: URLSearchParams,
  levels: readonly ScopeLevel[],
  directions: readonly Direction[],
): ThresholdFilters {
  return {
    q: readIdParam(params, 'q') ?? '',
    scopeLevel: readEnumParam(params, 'scope_level', ['', ...levels], ''),
    direction: readEnumParam(params, 'direction', ['', ...directions], ''),
  };
}

/** Write the filters back, deleting every key whose value is the default so the unfiltered view
 *  has no query string at all. */
export function writeFilters(params: URLSearchParams, f: ThresholdFilters): void {
  writeIdParam(params, 'q', f.q.trim() || null);
  writeEnumParam(params, 'scope_level', f.scopeLevel, '');
  writeEnumParam(params, 'direction', f.direction, '');
}
