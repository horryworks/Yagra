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
  normalizeSets,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { decodeCondition } from '../lib/filterCondition';
import { unset } from '../lib/filterQuery';
import { DIRECTIONS, SCOPE_LEVELS, type ScopeLevel, type StoredThreshold } from '../types/api';

/**
 * The scope levels this screen shows **before anyone filters** — every level except `interface`.
 *
 * Port rules are per (node × port × metric), so one 48-port switch contributes 96 rows for the two
 * traffic directions alone, and this list is capped at 500 server-side. Left in by default they
 * bury every other rule and then push the cap, at which point the screen stops showing rules it is
 * not hiding on purpose. They are one click away (the count and the control are above the table),
 * and the port's own dialog — Node detail ▸ Interfaces — is where they are normally read anyway.
 *
 * ⚠️ Derived from `SCOPE_LEVELS`, so a seventh level joins the default view rather than being
 * silently excluded by a hand-written list nobody revisits.
 */
export const DEFAULT_SCOPE_LEVELS: readonly ScopeLevel[] = SCOPE_LEVELS.filter(
  (l) => l !== 'interface',
);

/** The columns this module's functions read (ADR-053 Inc.10, 決定 AA).
 *
 *  This module was the clearest case for that decision: most of its 148 lines existed to carry
 *  three columns between two names for the same thing — `scope_level` ⟷ `scopeLevel` — plus a
 *  hand-written defaults object and a per-field URL codec that `useFilterParams` already had. What
 *  is left is the filter row's specs and the one mapping that is genuinely this screen's: which
 *  query parameter each column becomes. */
export type ThresholdColumns = readonly FilterableColumn<StoredThreshold>[];

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query string,
 * whereas `scope_level=` would arrive as an empty string and be rejected as an unknown level — a
 * filter nobody set turning into a 400.
 *
 * ⚠️ **`normalizeSets` is what keeps `?scope_level=galaxy` out of that same 400.** The URL is read
 * without validation on purpose (`readFilterParams`' rule: a stale bookmark opens the default view
 * rather than a broken control), so the token this screen does not offer has to be dropped here.
 */
export function queryFor(columns: ThresholdColumns, s: FilterState) {
  const f = normalizeSets(columns, s);
  return {
    // The text column stores an encoded condition; only its term reaches this API, which has
    // neither a regex parameter nor a negated form for `q`.
    q: unset(decodeCondition(f.q ?? '').term),
    scope_level: unset(f.scope_level),
    direction: unset(f.direction),
  };
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
 *
 * ⚠️ **Declared in the table's column order.** The page derives its filter list from this record
 * rather than from the columns — a display column closes over the resolved scope name, so deriving
 * from it would refetch the ruleset every time a name batch lands — and the mobile sheet lists
 * filters in the order it is given.
 */
export function thresholdFilters(t: TFunction): Record<string, ColumnFilterSpec<StoredThreshold>> {
  return {
    // ⚠️ **No column here carries a row accessor** (ADR-053 Inc.10). This list is capped at 500
    // server-side with a `truncated` flag, so a browser-side predicate would examine that prefix
    // and report on it — the exact failure the file header opens with, re-entered through the back
    // door. The absence is the declaration.
    scope_level: {
      kind: 'enum',
      options: SCOPE_LEVELS.map((l) => ({ value: l, label: t(`thresholds.scopeLevel.${l}`) })),
      allLabel: t('thresholds.filter.allScopes'),
      // The screen's default narrows (see `DEFAULT_SCOPE_LEVELS`). Stated on the spec rather than
      // as a `baseline` object threaded through three props, so the URL codec, the active-filter
      // count and the empty state all read the same answer and cannot disagree about whether the
      // default view is "filtered".
      defaultSelection: DEFAULT_SCOPE_LEVELS.join(','),
    },
    q: {
      kind: 'text',
      // Contains only — the endpoint does a substring match and has no regex parameter to carry a
      // pattern to. There is also no NOT: `q` has no negated form on the wire.
      modes: ['contains'],
      containsSemantics: 'substring',
      placeholder: t('thresholds.cols.metric'),
    },
    direction: {
      kind: 'enum',
      options: DIRECTIONS.map((d) => ({ value: d, label: t(`thresholds.direction.${d}`) })),
      allLabel: t('thresholds.filter.allDirections'),
    },
  };
}
