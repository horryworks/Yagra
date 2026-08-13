// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ History — the filter state and the request it becomes, as pure functions.
//
// Here rather than in the page because Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). Everything that decides *what is asked for* lives here; the page is layout.
//
// Paging itself lives in `historyCursor.ts` — it is correct on its own and was shipped on its own,
// because a cursor that skips rows is a bug whether or not anything is filtering.

import type { TFunction } from 'i18next';
import { decodeSet, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { isFiltered as isFilteredAgainst, sinceIso, unset } from '../lib/filterQuery';
import { readEnumParam, readIdParam, writeEnumParam, writeIdParam } from '../lib/filterParams';
import { severityLabel, stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { PAGE_SIZE } from './historyCursor';
import { SEVERITIES, type AlertHistoryRow, type NodeState, type Severity } from '../types/api';

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

/**
 * The Alerts ▸ History filter row, keyed by `Column.key` (ADR-053 Inc.4).
 *
 * ⚠️ **Every enum here is `single`, and that is the API's shape, not a UI preference.**
 * `GET /api/v1/alerts/history` takes one `severity`, one `state` and one `resolved`; a multi-select
 * over those would let an operator tick three boxes, send one, and see a list missing rows with
 * nothing on screen saying why. Widening the endpoint to comma-joined lists (as Events took in
 * Inc.2) is a backend increment with its own MCP-parity obligation — see the ADR.
 *
 * Columns deliberately left unfilterable: **node** (the ScopePicker in the action row answers it,
 * and it resolves names against the inventory — a different question from "does this cell contain
 * these characters"), **what** (the only free-text column is `metric`, unindexed on a table that
 * reaches millions of rows, so an ILIKE there turns the keyset seek into a seq scan) and **acked**.
 */
export function historyFilters(t: TFunction): Record<string, ColumnFilterSpec<AlertHistoryRow>> {
  return {
    // ⚠️ The keys are `severity` / `state` / `phase` / `range`, matching the query parameters this
    // screen has always used — the column keys were `sev` and `at`, and were renamed to these. That
    // is the cheap direction: a column key is internal, whereas the URL is a bookmark someone holds.
    severity: {
      kind: 'enum',
      single: true,
      options: SEVERITIES.map((s) => ({ value: s, label: severityLabel(s) })),
      // Server-side: the predicate is in SQL and these accessors are never called. They are here
      // because the spec type asks for them, and returning the real field keeps them honest if the
      // screen ever gains a client-side pass.
      readValue: (r) => r.severity,
      allLabel: t('history.filter.allSeverities'),
    },
    state: {
      kind: 'enum',
      single: true,
      options: SEVERITY_ORDER.map((s) => ({ value: s, label: stateLabel(s) })),
      readValue: (r) => r.state,
      allLabel: t('history.filter.allStates'),
    },
    phase: {
      kind: 'enum',
      single: true,
      options: [
        { value: 'fired', label: t('history.phase.fired') },
        { value: 'cleared', label: t('history.phase.cleared') },
      ],
      readValue: (r) => (r.resolved ? 'cleared' : 'fired'),
      allLabel: t('history.filter.allPhases'),
    },
    range: {
      kind: 'range',
      presets: HISTORY_RANGES.map((r) => ({
        value: r,
        label: t(`history.range.${r}`),
        // Server-side: the window becomes `since`, and a client predicate must not re-apply it
        // against a clock the server already used (`RangeFilterSpec.readTime`).
        seconds: null,
      })),
      defaultPreset: DEFAULT_FILTERS.range,
    },
  };
}

// The filter row's state is flat primitives keyed by column (`columnFilter.ts` explains why this is
// forced rather than chosen); `HistoryFilters` is named by what the API calls things and also
// carries the scope, which is not a column. These two functions are the only mapping between them.
//
// ⚠️ The **URL codec stays `readFilters`/`writeFilters`** below — the filter row does not get its
// own. Two codecs writing the same `URLSearchParams` is the shape that made "clear all filters" do
// nothing on the Events page (`useFilterParams.setFilters`): one handler, one write.

export function stateFromFilters(f: HistoryFilters): FilterState {
  return { severity: f.severity, state: f.state, phase: f.phase, range: f.range };
}

export function filtersFromState(
  s: FilterState,
  scope: { nodeId: string; groupId: string },
): HistoryFilters {
  // `single` columns still store a set, so a value arrives as a one-element token list.
  const one = (v: string | undefined) => decodeSet(v ?? '')[0] ?? '';
  const range = s.range ?? '';
  return {
    severity: one(s.severity) as HistoryFilters['severity'],
    state: one(s.state) as HistoryFilters['state'],
    phase: one(s.phase) as HistoryPhase,
    range: (HISTORY_RANGES as readonly string[]).includes(range)
      ? (range as HistoryRange)
      : DEFAULT_FILTERS.range,
    nodeId: scope.nodeId,
    groupId: scope.groupId,
  };
}

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
