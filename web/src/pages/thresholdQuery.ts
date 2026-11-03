// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Threshold rules — the view, the filter state and the request they become, as pure
// functions.
//
// **Server-side, and it has to be.** Thresholds are the one configuration table that grows with
// the fleet (a node-level override is per node × metric), which is why the list has always been
// capped at 500 with a `truncated` flag. A filter applied in the browser would run over that prefix
// only, so "show me the cpu_util rules" would silently examine the first 500 rules and report on
// those — exactly the failure Settings ▸ Audit had, on the table that decides when the fleet pages
// someone. The predicate goes in the query; `total` comes back counting the matches.
//
// **Since ADR-230 this screen is the one place a threshold rule is made or changed**, and the
// other screens link into it. That is why the view (which level, which target, which node is being
// checked) is in the URL and why the links are built here: a link that spelled the query by hand
// would be a second copy of these keys.
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
import { ALERT_RULES_PATH } from './alertRules';
import { thresholdFormFrom, type ThresholdForm } from './thresholdRequest';

/** The level tabs, in the order the precedence reads: every level, then broadest to narrowest. */
export const SCOPE_TABS = ['all', ...SCOPE_LEVELS] as const;
export type ScopeTab = (typeof SCOPE_TABS)[number];

/** The query keys the view owns. The filter columns own `q` and `direction`. */
export const VIEW_KEYS = { level: 'scope_level', target: 'scope_id', check: 'check' } as const;

/** What the screen is showing: a level tab and, for the three levels that name things, one target;
 *  or, with `check`, the rules that reach one node. */
export interface RuleView {
  tab: ScopeTab;
  /** A profile, folder or node id. Only kept on the tab that names that kind of thing. */
  target: string | null;
  /** A node id: show the rules that reach it rather than the list. */
  check: string | null;
}

/** What a tab's target is, or `null` for a tab that names nothing (every node, ports, all). */
export type TargetKind = 'profile' | 'folder' | 'node';

export function targetKind(tab: ScopeTab): TargetKind | null {
  switch (tab) {
    case 'profile':
      return 'profile';
    case 'group_id':
      return 'folder';
    case 'node':
      return 'node';
    case 'all':
    case 'global':
    case 'interface':
      return null;
  }
}

/** A target id worth sending. The API parses `scope_id` as a UUID and answers 400 to anything
 *  else, so a hand-edited URL opens the tab unnarrowed rather than as an error. */
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

function isTab(v: string): v is ScopeTab {
  return (SCOPE_TABS as readonly string[]).includes(v);
}

/** Read the view from the query string. An unknown or multi-valued level (a bookmark from before
 *  the tabs, `scope_level=global,profile`) opens **All** rather than a narrower view it guessed. */
export function ruleViewFrom(params: URLSearchParams): RuleView {
  const raw = params.get(VIEW_KEYS.level) ?? '';
  const tab: ScopeTab = isTab(raw) ? raw : 'all';
  const target = params.get(VIEW_KEYS.target);
  const check = params.get(VIEW_KEYS.check);
  return {
    tab,
    target: targetKind(tab) && target && UUID.test(target) ? target : null,
    check: check && UUID.test(check) ? check : null,
  };
}

/** Write the view into `params`, removing what it does not set so a link stays short. */
export function writeRuleView(params: URLSearchParams, view: RuleView): void {
  const set = (k: string, v: string | null) => (v ? params.set(k, v) : params.delete(k));
  set(VIEW_KEYS.level, view.tab === 'all' ? null : view.tab);
  set(VIEW_KEYS.target, targetKind(view.tab) ? view.target : null);
  set(VIEW_KEYS.check, view.check);
}

/** The link another screen puts on a profile, a folder or a node (ADR-230 decisions 4 and 5). */
export function thresholdRulesHref(view: Partial<RuleView>): string {
  const params = new URLSearchParams();
  writeRuleView(params, { tab: 'all', target: null, check: null, ...view });
  const qs = params.toString();
  return qs ? `${ALERT_RULES_PATH}?${qs}` : ALERT_RULES_PATH;
}

/** The add dialog's opening state: the tab's level, and the chosen target already picked. On
 *  **All** it is the dialog's own default, because no level was chosen. */
export function startFormFor(view: RuleView): ThresholdForm | undefined {
  if (view.tab === 'all') return undefined;
  return {
    ...thresholdFormFrom(),
    level: view.tab,
    scopeIds: view.target && targetKind(view.tab) ? [view.target] : [],
  };
}

/** The columns this module's functions read (ADR-053 Inc.10, decision AA). */
export type ThresholdColumns = readonly FilterableColumn<StoredThreshold>[];

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query string,
 * whereas `scope_level=` would arrive as an empty string and be rejected as an unknown level — a
 * filter nobody set turning into a 400.
 *
 * ⚠️ **`normalizeSets` is what keeps `?direction=sideways` out of that same 400.** The URL is read
 * without validation on purpose (`readFilterParams`' rule: a stale bookmark opens the default view
 * rather than a broken control), so the token this screen does not offer has to be dropped here.
 */
export function queryFor(columns: ThresholdColumns, s: FilterState, view: RuleView) {
  const f = normalizeSets(columns, s);
  const level: ScopeLevel | undefined = view.tab === 'all' ? undefined : view.tab;
  return {
    // The text column stores an encoded condition; only its term reaches this API, which has
    // neither a regex parameter nor a negated form for `q`.
    q: unset(decodeCondition(f.q ?? '').term),
    scope_level: level,
    scope_id: view.target ?? undefined,
    direction: unset(f.direction),
  };
}

/**
 * The Alerts ▸ Threshold rules filters beside the column headers, keyed by `Column.key`.
 *
 * ⚠️ The keys are the query parameters this screen already used (`q` / `direction`), so bookmarks
 * taken before the filter row shipped still resolve. The level is **not** here since ADR-230: the
 * tabs above the table own it, one level at a time, and a second control over the same key would
 * be two answers to which levels are shown.
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
