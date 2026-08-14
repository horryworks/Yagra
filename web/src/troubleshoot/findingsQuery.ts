// SPDX-License-Identifier: AGPL-3.0-only
// All findings — the query logic behind `/troubleshoot/findings`, as pure functions.
// (The screen was "Saved findings" until 2026-08-14; `SavedFindingsPage.tsx` records why the code
// name kept the old word and the label did not.)
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.

import type { SavedFinding, SavedFindingsQuery } from '../types/api';
import type { ScopeValue } from '../components/ScopePicker/scope';
import type { TFunction } from 'i18next';
import {
  decodeNumberRange,
  decodeSet,
  encodeNumberRange,
  encodeSet,
  type ColumnFilterSpec,
  type FilterState,
} from '../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../lib/filterCondition';
import { sinceIso as sinceIsoFor } from '../lib/filterQuery';
import { rangePresets, rangeSeconds, type RangeToken } from '../lib/filterPresets';
import { FINDING_SEVERITIES } from '../types/api';
import { TOOLS } from './data';

/**
 * Rows per request. Matches the backend's default and stays under its 200 ceiling, so a page is
 * one round trip and "a short page" is a reliable end-of-results signal — see [`nextCursor`].
 */
export const PAGE_SIZE = 100;

/** The time windows the screen offers. The lengths are `filterPresets.ts`'s (ADR-053 Inc.10);
 *  `satisfies` is what makes a token added here without one there a compile error. */
export const FINDING_RANGES = ['24h', '7d', '30d', 'all'] as const satisfies readonly RangeToken[];
export type FindingRange = (typeof FINDING_RANGES)[number];

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
  /**
   * The score interval, encoded as `min:max` — the `number` column kind's transport (ADR-053
   * Inc.6). Kept as the encoded **string** rather than as two numbers so that it stays a primitive:
   * it is a `useEffect` dependency, and a `{min, max}` object would compare by reference and refetch
   * the first page on every render.
   */
  score: string;
  range: FindingRange;
  nodeId: string;
  groupId: string;
}

/**
 * The default view: a week, unfiltered.
 *
 * Not `all`. An unbounded default would get slower as the table fills, and the first screen an
 * operator opens is the wrong place to discover that.
 *
 * ⚠️ This used to say "nothing prunes `analysis_findings`", and that is no longer true — findings
 * cascade from `analysis_jobs`, which `retention::Subject::AnalysisRuns` trims at `diagnostic_days`
 * (default 90). The *ceiling* is therefore bounded; the default stays 7d anyway because scheduled
 * analyses fill 90 days steadily and the operator's question is almost always "this week".
 */
export const DEFAULT_FILTERS: FindingFilters = {
  tool: '',
  severity: '',
  q: '',
  nodeQ: '',
  score: '',
  range: '7d',
  nodeId: '',
  groupId: '',
};

/** Keyset cursor: the `at`/`id` of the last row already held. */
export interface FindingCursor {
  before: string;
  before_id: string;
}

/** The `since` bound for a range, or `undefined` for "all time".
 *
 *  Kept as a screen-local name because callers pass a `FindingRange`, but it holds no arithmetic of
 *  its own any more (Inc.10): the length comes from `filterPresets.ts` and the instant from
 *  `filterQuery.ts`. It used to be a private seconds table plus a hand-rolled subtraction — the
 *  fifth copy of both. */
export function sinceIso(range: FindingRange, nowMs: number): string | undefined {
  return sinceIsoFor(rangeSeconds(range), nowMs);
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
  const score = decodeNumberRange(f.score);
  return {
    tool: f.tool || undefined,
    severity: f.severity || undefined,
    q: f.q || undefined,
    node_id: f.nodeId || undefined,
    node_q: f.nodeQ || undefined,
    group_id: f.groupId || undefined,
    // `?? undefined` and not `|| undefined`: a bound of **zero** is a real filter on a score whose
    // floor is zero, and `||` would drop it — the same trap the codec's own tests pin.
    min_score: score.min ?? undefined,
    max_score: score.max ?? undefined,
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
    f.score !== '' ||
    f.nodeId !== '' ||
    f.groupId !== '' ||
    f.range !== DEFAULT_FILTERS.range
  );
}

/**
 * The Troubleshoot ▸ All findings filter row, keyed by `Column.key` (ADR-053 Inc.4).
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
 * unsayable. **Score closed the last one in Inc.6**, and it needed a new filter *kind* rather than a
 * new parameter shape — which is why it came a increment later than the other two.
 *
 * ⚠️ The range default is `7d`, not `all`, and that is a performance contract rather than a
 * preference — see `DEFAULT_FILTERS` above. `clientRangePresets` is still not reused, but only
 * because this screen offers a different subset and a different label namespace — the *lengths*
 * come from `filterPresets.ts` like everyone else's since Inc.10. The reason written here before
 * ("these presets carry `seconds: null` because the window is applied by the server") was the
 * belief that produced five private seconds tables; what actually keeps the window out of the
 * browser is the absent `readTime`, which `filterPredicate.ts` checks first.
 */
export function findingFilters(t: TFunction): Record<string, ColumnFilterSpec<SavedFinding>> {
  return {
    // ⚠️ **No column here carries a row accessor**, which is what `score` below already says for
    // its own kind and what Inc.10 made sayable for the other four: this list is answered by the
    // server, so a browser-side predicate would re-filter one keyset page and hide older matches.
    // Half a filter row running client-side and half server-side is worse than either.
    severity: {
      kind: 'enum',
      options: FINDING_SEVERITIES.map((s) => ({
        value: s,
        label: t(`findings.severity.${s}`),
      })),
      allLabel: t('findings.filter.allSeverities'),
    },
    tool: {
      kind: 'enum',
      // From the same catalog the launcher offers, so a new analysis appears here with no second
      // list to remember.
      options: TOOLS.map((tool) => ({ value: tool.id, label: t(tool.name) })),
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
      // browser-side pass disagree with both, so there is deliberately nothing to read — and since
      // Inc.10 that is spelled as an absent `readText` rather than as one returning `[]`, which was
      // a predicate rejecting every row waiting for someone to call it.
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
      containsSemantics: 'substring',
      placeholder: t('findings.cols.what'),
    },
    // Scores run 0–100 and higher is worse, so the useful question is nearly always one-sided
    // ("60 and up"). No `readNumber`: the bounds go to the server as `min_score`/`max_score`, and a
    // browser-side pass would re-filter a keyset page the server already filtered.
    score: {
      kind: 'number',
      min: 0,
      max: 100,
      step: 1,
    },
    range: {
      kind: 'range',
      presets: rangePresets(FINDING_RANGES, t, 'findings.range.'),
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
    // Already the column's own encoding, so it passes straight through in both directions.
    score: f.score,
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
    // Re-encoded rather than copied: a hand-typed URL may say `03:` or ` 3 : 5 `, and normalising
    // here keeps the value stable as an effect dependency the way the set columns do.
    score: encodeNumberRange(decodeNumberRange(s.score ?? '')),
    range: (FINDING_RANGES as readonly string[]).includes(range)
      ? (range as FindingRange)
      : DEFAULT_FILTERS.range,
    nodeId: scope.nodeId,
    groupId: scope.groupId,
  };
}
