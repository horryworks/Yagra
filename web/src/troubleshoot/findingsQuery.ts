// SPDX-License-Identifier: AGPL-3.0-only
// Saved findings — the query logic behind `/troubleshoot/findings`, as pure functions.
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.

import type {
  AnalysisToolKey,
  FindingSeverity,
  SavedFinding,
  SavedFindingsQuery,
} from '../types/api';
import type { ScopeValue } from '../components/ScopePicker/scope';

/**
 * Rows per request. Matches the backend's default and stays under its 200 ceiling, so a page is
 * one round trip and "a short page" is a reliable end-of-results signal — see [`nextCursor`].
 */
export const PAGE_SIZE = 100;

/** The time windows the screen offers. */
export const FINDING_RANGES = ['24h', '7d', '30d', 'all'] as const;
export type FindingRange = (typeof FINDING_RANGES)[number];

const RANGE_SECS: Record<FindingRange, number | null> = {
  '24h': 86_400,
  '7d': 7 * 86_400,
  '30d': 30 * 86_400,
  all: null,
};

/** The screen's filter state. `''` means "no filter" for each optional field. */
export interface FindingFilters {
  tool: AnalysisToolKey | '';
  severity: FindingSeverity | '';
  range: FindingRange;
  nodeId: string;
  groupId: string;
}

/**
 * The default view: a week, unfiltered.
 *
 * Not `all`. The findings table only grows — nothing prunes `analysis_findings` today — so an
 * unbounded default would get slower for the life of the deployment, and the first screen an
 * operator opens is the wrong place to discover that.
 */
export const DEFAULT_FILTERS: FindingFilters = {
  tool: '',
  severity: '',
  range: '7d',
  nodeId: '',
  groupId: '',
};

/** Keyset cursor: the `at`/`id` of the last row already held. */
export interface FindingCursor {
  before: string;
  before_id: string;
}

/** The `since` bound for a range, or `undefined` for "all time". */
export function sinceIso(range: FindingRange, nowMs: number): string | undefined {
  const secs = RANGE_SECS[range];
  return secs === null ? undefined : new Date(nowMs - secs * 1000).toISOString();
}

/**
 * The request for one page.
 *
 * Every unset filter is `undefined`, never `''`: the client drops `undefined` from the query
 * string, whereas `severity=` would reach the backend as an empty string and be rejected as an
 * unknown severity — a filter nobody set turning into a 400.
 */
export function queryFor(
  f: FindingFilters,
  cursor: FindingCursor | null,
  nowMs: number,
): SavedFindingsQuery {
  return {
    tool: f.tool || undefined,
    severity: f.severity || undefined,
    node_id: f.nodeId || undefined,
    group_id: f.groupId || undefined,
    since: sinceIso(f.range, nowMs),
    before: cursor?.before,
    before_id: cursor?.before_id,
    limit: PAGE_SIZE,
  };
}

/**
 * The cursor for the page after `rows`, or `null` when there is no next page.
 *
 * Both halves of the cursor come from the same row. Sending `before` alone would re-request every
 * finding written in that row's millisecond — a run inserts its findings in a tight loop, so that
 * is a real set, not a theoretical one — and the page would appear to repeat itself.
 */
export function nextCursor(rows: SavedFinding[]): FindingCursor | null {
  const last = rows.at(-1);
  if (rows.length < PAGE_SIZE || !last) return null;
  return { before: last.at, before_id: last.id };
}

/**
 * Append a page to what is already held, dropping rows already present.
 *
 * Defensive rather than expected: the cursor is total, so a duplicate should be impossible. But a
 * duplicate `key` in React is a rendering bug rather than a visible one, and the cost of being
 * wrong about "impossible" here is a screen that silently misrenders.
 */
export function appendPage(have: SavedFinding[], page: SavedFinding[]): SavedFinding[] {
  const seen = new Set(have.map((f) => f.id));
  return [...have, ...page.filter((f) => !seen.has(f.id))];
}

/**
 * Split a `ScopeValue` from the shared [`ScopePicker`] into the two filter fields.
 *
 * The picker is reused rather than re-solved: it already answers "all / this group / this node"
 * with a server-side node typeahead, which is the same question this screen asks. The two fields
 * stay separate because the backend treats them differently — a group means its whole subtree, a
 * node means exactly one — so collapsing them into one id would lose which was meant.
 */
export function scopeFilter(scope: ScopeValue): Pick<FindingFilters, 'nodeId' | 'groupId'> {
  return {
    nodeId: scope.kind === 'node' ? (scope.id ?? '') : '',
    groupId: scope.kind === 'group' ? (scope.id ?? '') : '',
  };
}

/** Whether anything is narrowing the search — drives the empty state's wording. */
export function isFiltered(f: FindingFilters): boolean {
  return (
    f.tool !== '' ||
    f.severity !== '' ||
    f.nodeId !== '' ||
    f.groupId !== '' ||
    f.range !== DEFAULT_FILTERS.range
  );
}
