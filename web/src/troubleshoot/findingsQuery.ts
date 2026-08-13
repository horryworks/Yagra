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
import type { TFunction } from 'i18next';
import { decodeSet, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { FINDING_SEVERITIES } from '../types/api';
import { TOOLS } from './data';

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

/**
 * The Troubleshoot ▸ Saved findings filter row, keyed by `Column.key` (ADR-053 Inc.4).
 *
 * The keys are the API's parameter names (`tool` / `severity` / `range`); the columns were `tool`,
 * `severity` and `at`, so only the time column was renamed.
 *
 * Both enums are `single` because `GET /api/v1/analysis/findings` takes one of each. The Node and
 * What columns carry no filter: the scope is answered by the ScopePicker in the action row (it
 * resolves names against the inventory, which is a different question from "does this cell contain
 * these characters"), and the finding text has no server-side search parameter to send it to.
 *
 * ⚠️ The range default is `7d`, not `all`, and that is a performance contract rather than a
 * preference — see `DEFAULT_FILTERS` above. `clientRangePresets` is deliberately not reused: these
 * presets carry `seconds: null` because the window is applied by the server.
 */
export function findingFilters(t: TFunction): Record<string, ColumnFilterSpec<SavedFinding>> {
  return {
    severity: {
      kind: 'enum',
      single: true,
      options: FINDING_SEVERITIES.map((s) => ({
        value: s,
        label: t(`findings.severity.${s}`),
      })),
      readValue: (f) => f.severity,
      allLabel: t('findings.filter.allSeverities'),
    },
    tool: {
      kind: 'enum',
      single: true,
      // From the same catalog the launcher offers, so a new analysis appears here with no second
      // list to remember.
      options: TOOLS.map((tool) => ({ value: tool.id, label: t(tool.name) })),
      readValue: (f) => f.tool,
      allLabel: t('findings.filter.allTools'),
    },
    range: {
      kind: 'range',
      presets: FINDING_RANGES.map((r) => ({
        value: r,
        label: t(`findings.range.${r}`),
        seconds: null,
      })),
      defaultPreset: DEFAULT_FILTERS.range,
    },
  };
}

/** The flat row state ⟷ the filter shape. The scope is not a column, so it is carried through. */
export function stateFromFilters(f: FindingFilters): FilterState {
  return { severity: f.severity, tool: f.tool, range: f.range };
}

export function filtersFromState(
  s: FilterState,
  scope: { nodeId: string; groupId: string },
): FindingFilters {
  const one = (v: string | undefined) => decodeSet(v ?? '')[0] ?? '';
  const range = s.range ?? '';
  return {
    tool: one(s.tool) as FindingFilters['tool'],
    severity: one(s.severity) as FindingFilters['severity'],
    range: (FINDING_RANGES as readonly string[]).includes(range)
      ? (range as FindingRange)
      : DEFAULT_FILTERS.range,
    nodeId: scope.nodeId,
    groupId: scope.groupId,
  };
}
