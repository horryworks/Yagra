// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ History — the filter state and the request it becomes, as pure functions.
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.
//
// Paging itself lives in `historyCursor.ts` — it is correct on its own and was shipped on its own,
// because a cursor that skips rows is a bug whether or not anything is filtering.

import { isFiltered as isFilteredAgainst, sinceIso, unset } from '../lib/filterQuery';
import { readEnumParam, readIdParam, writeEnumParam, writeIdParam } from '../lib/filterParams';
import { PAGE_SIZE } from './historyCursor';
import type { NodeState, Severity } from '../types/api';

/** The lifecycle phases the screen offers, mapped to the API's `resolved` boolean. */
export const HISTORY_PHASES = ['', 'fired', 'cleared'] as const;
export type HistoryPhase = (typeof HISTORY_PHASES)[number];

/** The time windows the screen offers. */
export const HISTORY_RANGES = ['24h', '7d', '30d', 'all'] as const;
export type HistoryRange = (typeof HISTORY_RANGES)[number];

const RANGE_SECS: Record<HistoryRange, number | null> = {
  '24h': 86_400,
  '7d': 7 * 86_400,
  '30d': 30 * 86_400,
  all: null,
};

/** The screen's filter state. `''` means "no filter" for each optional field. */
export interface HistoryFilters {
  severity: Severity | '';
  state: NodeState | '';
  phase: HistoryPhase;
  range: HistoryRange;
  nodeId: string;
  groupId: string;
}

/**
 * The default view: everything, unfiltered.
 *
 * `all` rather than a bounded window. `alert_history_cursor_idx` orders the table by exactly the
 * columns the cursor pages on, so an unfiltered first page is an index seek of 100 rows however
 * large the log is — and History showing everything on open is what it has always done. (Saved
 * findings defaults to 7d for the opposite reason: nothing prunes that table and it has no such
 * index.)
 */
export const DEFAULT_FILTERS: HistoryFilters = {
  severity: '',
  state: '',
  phase: '',
  range: 'all',
  nodeId: '',
  groupId: '',
};

/** `resolved` for a phase: `undefined` means "both", which is not the same as `false`. */
export function resolvedFor(phase: HistoryPhase): boolean | undefined {
  if (phase === 'fired') return false;
  if (phase === 'cleared') return true;
  return undefined;
}

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query string,
 * whereas `severity=` would reach the backend as an empty string and be rejected as an unknown
 * severity — a filter nobody set turning into a 400.
 *
 * `nowMs` is a parameter so a relative range is testable without faking the clock.
 */
export function queryFor(
  f: HistoryFilters,
  cursor: { before: string; before_id: string } | null,
  nowMs: number,
) {
  return {
    limit: PAGE_SIZE,
    severity: unset(f.severity),
    state: unset(f.state),
    resolved: resolvedFor(f.phase),
    node_id: unset(f.nodeId),
    group_id: unset(f.groupId),
    since: sinceIso(RANGE_SECS[f.range], nowMs),
    before: cursor?.before,
    before_id: cursor?.before_id,
  };
}

/** Whether anything is narrowing the log — drives the empty state's wording.
 *
 *  ⚠️ Must not be replaced by a `rows.length` check: with the filter in SQL, a filtered query that
 *  legitimately returns zero is indistinguishable from an empty log. */
export function isFiltered(f: HistoryFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_FILTERS);
}

/**
 * Read the filters out of the URL.
 *
 * The scope ids are the reason this screen carries its filters in the URL at all: a node page
 * linking to "this node's alert history" needs somewhere to say which node, and nothing else holds
 * that. The rest ride along so a filtered view can be shared and survives a reload.
 */
export function readFilters(params: URLSearchParams, severities: readonly Severity[], states: readonly NodeState[]): HistoryFilters {
  return {
    severity: readEnumParam(params, 'severity', ['', ...severities], ''),
    state: readEnumParam(params, 'state', ['', ...states], ''),
    phase: readEnumParam(params, 'phase', HISTORY_PHASES, ''),
    range: readEnumParam(params, 'range', HISTORY_RANGES, DEFAULT_FILTERS.range),
    nodeId: readIdParam(params, 'node_id') ?? '',
    groupId: readIdParam(params, 'group_id') ?? '',
  };
}

/** Write the filters back, deleting every key whose value is the default so the unfiltered view
 *  has no query string at all. */
export function writeFilters(params: URLSearchParams, f: HistoryFilters): void {
  writeEnumParam(params, 'severity', f.severity, '');
  writeEnumParam(params, 'state', f.state, '');
  writeEnumParam(params, 'phase', f.phase, '');
  writeEnumParam(params, 'range', f.range, DEFAULT_FILTERS.range);
  writeIdParam(params, 'node_id', f.nodeId || null);
  writeIdParam(params, 'group_id', f.groupId || null);
}
