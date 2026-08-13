// SPDX-License-Identifier: AGPL-3.0-only
// Saved findings — the query logic behind `/troubleshoot/findings`, as pure functions.
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.

import type { SavedFinding, SavedFindingsQuery } from '../types/api';
import type { ScopeValue } from '../components/ScopePicker/scope';
import type { TFunction } from 'i18next';
import {
  decodeSet,
  encodeSet,
  type ColumnFilterSpec,
  type FilterState,
} from '../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../lib/filterCondition';
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
  /** Comma-joined `AnalysisToolKey` tokens; `''` is every tool. Same spelling as the API takes. */
  tool: string;
  /** Comma-joined `FindingSeverity` tokens; `''` is every severity. */
  severity: string;
  /** Substring of the metric name or the finding kind (`q`). */
  q: string;
  /** Substring of the node's current name (`node_q`). */
  nodeQ: string;
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
  q: '',
  nodeQ: '',
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
    q: f.q || undefined,
    node_id: f.nodeId || undefined,
    node_q: f.nodeQ || undefined,
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
    f.q !== '' ||
    f.nodeQ !== '' ||
    f.nodeId !== '' ||
    f.groupId !== '' ||
    f.range !== DEFAULT_FILTERS.range
  );
}

/**
 * The Troubleshoot ▸ Saved findings filter row, keyed by `Column.key` (ADR-053 Inc.4).
 *
 * The keys are the API's parameter names (`tool` / `severity` / `q` / `node_q` / `range`); the
 * columns were `tool`, `severity`, `node`, `what` and `at`, so three were renamed.
 *
 * Both enums are multi-select: since ADR-053 Inc.4b `GET /api/v1/analysis/findings` takes `tool`
 * and `severity` as comma-separated sets.
 *
 * ⚠️ **Node and What had no filter until Inc.4b because the API had no parameter for either.**
 * That distinction is invisible from the screen — a user reported both as missing — so the fix was
 * to add the parameters rather than to explain the absence. The Node filter is *not* a duplicate of
 * the action row's ScopePicker: that selects exactly one node, and "every node called core-sw…" was
 * unsayable. **Score is still unfiltered**, and that one is a genuine gap: a numeric range is a
 * filter *kind* this row does not have yet (ADR-053 Inc.6).
 *
 * ⚠️ The range default is `7d`, not `all`, and that is a performance contract rather than a
 * preference — see `DEFAULT_FILTERS` above. `clientRangePresets` is deliberately not reused: these
 * presets carry `seconds: null` because the window is applied by the server.
 */
export function findingFilters(t: TFunction): Record<string, ColumnFilterSpec<SavedFinding>> {
  return {
    severity: {
      kind: 'enum',
      options: FINDING_SEVERITIES.map((s) => ({
        value: s,
        label: t(`findings.severity.${s}`),
      })),
      readValue: (f) => f.severity,
      allLabel: t('findings.filter.allSeverities'),
    },
    tool: {
      kind: 'enum',
      // From the same catalog the launcher offers, so a new analysis appears here with no second
      // list to remember.
      options: TOOLS.map((tool) => ({ value: tool.id, label: t(tool.name) })),
      readValue: (f) => f.tool,
      allLabel: t('findings.filter.allTools'),
    },
    // The node's **current** name, matched as a substring server-side (`node_q`). A fleet-wide
    // finding has no node and therefore never matches — which is right, it is not about one.
    node_q: {
      kind: 'text',
      // Contains only: `node_q` is a substring parameter with neither a regex nor a negated form.
      modes: ['contains'],
      // The row's `node_name` is the name **as of the run**, while the column renders the current
      // one and the server matches the current one. Reading the stale copy here would make a
      // browser-side pass disagree with both, so there is deliberately nothing to read.
      readText: () => [],
      containsSemantics: 'substring',
      placeholder: t('findings.cols.node'),
    },
    // The What column shows the metric **and** the kind, so `q` matches both — the same shape as
    // the audit log's `q` over username and action, and the same imprecision: a term can match on
    // the half the operator was not thinking of. Better than matching only the half that fits in
    // one parameter and silently dropping rows that match the other.
    q: {
      kind: 'text',
      modes: ['contains'],
      readText: (f) => [f.metric, f.kind],
      containsSemantics: 'substring',
      placeholder: t('findings.cols.what'),
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
  return {
    severity: f.severity,
    tool: f.tool,
    node_q: f.nodeQ ? encodeCondition({ term: f.nodeQ, mode: 'contains', not: false }) : '',
    q: f.q ? encodeCondition({ term: f.q, mode: 'contains', not: false }) : '',
    range: f.range,
  };
}

export function filtersFromState(
  s: FilterState,
  scope: { nodeId: string; groupId: string },
): FindingFilters {
  const range = s.range ?? '';
  return {
    // Set columns store their tokens joined and the API takes the same spelling; re-encoding pins
    // the order so the value is stable as an effect dependency.
    tool: encodeSet(
      decodeSet(s.tool ?? ''),
      TOOLS.map((t) => t.id),
    ),
    severity: encodeSet(decodeSet(s.severity ?? ''), FINDING_SEVERITIES),
    // Text columns store an encoded condition; only the term reaches this API (neither parameter
    // has a regex or a negated form).
    q: decodeCondition(s.q ?? '').term,
    nodeQ: decodeCondition(s.node_q ?? '').term,
    range: (FINDING_RANGES as readonly string[]).includes(range)
      ? (range as FindingRange)
      : DEFAULT_FILTERS.range,
    nodeId: scope.nodeId,
    groupId: scope.groupId,
  };
}
