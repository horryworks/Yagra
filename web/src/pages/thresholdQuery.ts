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

import type { TFunction } from 'i18next';
import {
  decodeSet,
  encodeSet,
  type ColumnFilterSpec,
  type FilterState,
} from '../lib/columnFilter';
import { decodeCondition } from '../lib/filterCondition';
import { isFiltered as isFilteredAgainst, unset } from '../lib/filterQuery';
import { readIdParam, readSetParam, writeIdParam, writeSetParam } from '../lib/filterParams';
import {
  DIRECTIONS,
  SCOPE_LEVELS,
  type Direction,
  type ScopeLevel,
  type StoredThreshold,
} from '../types/api';

/** The screen's filter state. `''` means "no filter" for each field. */
export interface ThresholdFilters {
  /** Free text over the metric name — matched as a substring, server-side. */
  q: string;
  /** Comma-joined `ScopeLevel` tokens; `''` is every level. Same spelling as the API takes. */
  scopeLevel: string;
  /** Comma-joined `Direction` tokens; `''` is both. */
  direction: string;
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
    // ⚠️ `readSetParam`, not `readEnumParam`: these carry several tokens since Inc.4b. A bookmark
    // holding one (`?scope_level=node`) still reads correctly — one token is a one-element set.
    scopeLevel: readSetParam(params, 'scope_level', levels),
    direction: readSetParam(params, 'direction', directions),
  };
}

/** Write the filters back, deleting every key whose value is the default so the unfiltered view
 *  has no query string at all. */
export function writeFilters(params: URLSearchParams, f: ThresholdFilters): void {
  writeIdParam(params, 'q', f.q.trim() || null);
  writeSetParam(params, 'scope_level', f.scopeLevel);
  writeSetParam(params, 'direction', f.direction);
}

/**
 * The Alerts ▸ Thresholds filter row, keyed by `Column.key` (ADR-053 Inc.4).
 *
 * ⚠️ The keys are the query parameters this screen already used (`q` / `scope_level` /
 * `direction`), so bookmarks taken before the filter row shipped still resolve — the columns were
 * `scope`, `metric` and `direction`, and two were renamed. A column key is internal; a saved URL
 * is not.
 *
 * Both enums are multi-select: since ADR-053 Inc.4b `GET /api/v1/thresholds` takes `scope_level`
 * and `direction` as comma-separated sets. The URL therefore carries a *joined* value where it used
 * to carry one token — `scope_level=group,node` — which older bookmarks (`scope_level=node`) still
 * read correctly, since one token is a one-element set.
 */
export function thresholdFilters(t: TFunction): Record<string, ColumnFilterSpec<StoredThreshold>> {
  return {
    q: {
      kind: 'text',
      // Contains only — the endpoint does a substring match and has no regex parameter to carry a
      // pattern to. There is also no NOT: `q` has no negated form on the wire.
      modes: ['contains'],
      readText: (r) => [r.metric],
      containsSemantics: 'substring',
      placeholder: t('thresholds.cols.metric'),
    },
    scope_level: {
      kind: 'enum',
      options:SCOPE_LEVELS.map((l) => ({ value: l, label: t(`thresholds.scopeLevel.${l}`) })),
      readValue: (r) => r.scope_level,
      allLabel: t('thresholds.filter.allScopes'),
    },
    direction: {
      kind: 'enum',
      options:DIRECTIONS.map((d) => ({ value: d, label: t(`thresholds.direction.${d}`) })),
      readValue: (r) => r.direction,
      allLabel: t('thresholds.filter.allDirections'),
    },
  };
}

/** The flat row state ⟷ the request shape. The URL codec stays `readFilters`/`writeFilters`; the
 *  filter row does not get a second one writing the same params (one handler, one write). */
export function stateFromFilters(f: ThresholdFilters): FilterState {
  return { q: f.q, scope_level: f.scopeLevel, direction: f.direction };
}

export function filtersFromState(s: FilterState): ThresholdFilters {
  // A set column stores its tokens joined and the API takes the same spelling, so both are
  // pass-throughs — re-encoded only to pin the order, which keeps the URL comparable.
  return {
    q: decodeCondition(s.q ?? '').term,
    scopeLevel: encodeSet(decodeSet(s.scope_level ?? ''), SCOPE_LEVELS),
    direction: encodeSet(decodeSet(s.direction ?? ''), DIRECTIONS),
  };
}
