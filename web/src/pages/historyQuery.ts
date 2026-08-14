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
import {
  decodeSet,
  encodeSet,
  type ColumnFilterSpec,
  type FilterState,
} from '../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../lib/filterCondition';
import { isFiltered as isFilteredAgainst, sinceIso, unset } from '../lib/filterQuery';
import { rangePresets, rangeSeconds, type RangeToken } from '../lib/filterPresets';
import {
  readEnumParam,
  readIdParam,
  readSetParam,
  writeEnumParam,
  writeIdParam,
  writeSetParam,
} from '../lib/filterParams';
import { severityLabel, stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { PAGE_SIZE } from './historyCursor';
import { SEVERITIES, type AlertHistoryRow, type NodeState, type Severity } from '../types/api';

/** The lifecycle phases the screen offers, mapped to the API's `resolved` boolean. */
export const HISTORY_PHASES = ['', 'fired', 'cleared'] as const;
export type HistoryPhase = (typeof HISTORY_PHASES)[number];

/** The time windows the screen offers. The lengths are `filterPresets.ts`'s (ADR-053 Inc.10);
 *  `satisfies` is what makes a token added here without one there a compile error. */
export const HISTORY_RANGES = ['24h', '7d', '30d', 'all'] as const satisfies readonly RangeToken[];
export type HistoryRange = (typeof HISTORY_RANGES)[number];

/**
 * The Alerts ▸ History filter row, keyed by `Column.key` (ADR-053 Inc.4 / Inc.4b).
 *
 * The enums are multi-select: since Inc.4b `GET /api/v1/alerts/history` takes `severity` and
 * `state` as comma-separated sets. `phase` stays effectively single-valued because it maps onto a
 * *boolean* (`resolved`) — ticking both fired and cleared is the unfiltered view, and
 * [`resolvedFor`] returns `undefined` for it, which is exactly right rather than a special case.
 *
 * ⚠️ **The node / what / acked columns had no filter until Inc.4b, and the reason mattered: none
 * of the three could be asked of the API.** A user reported all three as missing twice, which is
 * the real lesson — *from the screen there is no difference between "deliberately absent" and
 * "forgotten"*, so a column with no filter looks like a bug whatever the reason. Each got a
 * parameter rather than a browser-side predicate, because filtering a keyset-paged list in the
 * browser hides older matches while looking like it worked.
 */
export function historyFilters(t: TFunction): Record<string, ColumnFilterSpec<AlertHistoryRow>> {
  return {
    // ⚠️ The keys are the query parameters this screen has always used — the column keys were `sev`
    // and `at`, and were renamed to these. That is the cheap direction: a column key is internal,
    // whereas the URL is a bookmark someone holds.
    // ⚠️ **Not one column here carries a row accessor, and that absence IS the declaration that
    // this list is answered by the server** (ADR-053 Inc.10). They used to be supplied "because the
    // spec type asks for them"; Inc.8 made four of the five optional and Inc.10 made the last one
    // optional, so the reason expired twice over. Supplying one on a server-side column is not
    // free: it arms a browser-side predicate that runs over **one keyset page**, so the moment
    // anything on this screen calls `applyFilters` it would hide older rows the query legitimately
    // returned — and only for the columns that happen to have an accessor, which is the worst
    // shape (half the filter row silently means something different from the other half).
    severity: {
      kind: 'enum',
      options: SEVERITIES.map((s) => ({ value: s, label: severityLabel(s) })),
      allLabel: t('history.filter.allSeverities'),
    },
    state: {
      kind: 'enum',
      options: SEVERITY_ORDER.map((s) => ({ value: s, label: stateLabel(s) })),
      allLabel: t('history.filter.allStates'),
    },
    phase: {
      kind: 'enum',
      options: [
        { value: 'fired', label: t('history.phase.fired') },
        { value: 'cleared', label: t('history.phase.cleared') },
      ],
      allLabel: t('history.filter.allPhases'),
    },
    // The node's **current** name, matched as a substring by the server (`node_q`). Deliberately
    // not the same question as the action row's ScopePicker, which selects exactly one node — "every
    // node called core-sw…" could not be asked at all before this.
    node_q: {
      kind: 'text',
      // Contains only: `node_q` is a substring parameter with no regex and no negated form. Offering
      // either toggle would promise something there is nothing to send.
      modes: ['contains'],
      // ⚠️ **No `readText`, and this column is why the accessor became optional** (Inc.10). There is
      // nothing honest to read: the row carries the subject's **id**, and the name it displays is
      // resolved separately through `useEntityNames`. It used to return `[]` to satisfy a required
      // field — a predicate that rejects every row, sitting one `useClientFilters` call away from
      // blanking the page. The predicate is the server's, against `nodes.name`.
      containsSemantics: 'substring',
      placeholder: t('history.cols.node'),
    },
    // The metric name, matched as a substring by the server (`metric`).
    //
    // ⚠️ **A more selective term is the slower one here**, which is the opposite of the intuition:
    // the index still serves the ordering and the page size, so the planner walks it until the page
    // is full — a metric matching one row in a million walks the whole index. Liveness transitions
    // store no metric and never match.
    metric: {
      kind: 'text',
      modes: ['contains'],
      containsSemantics: 'substring',
      placeholder: t('history.cols.what'),
    },
    // Whether the incident was acknowledged. The question this answers — "what fired this week that
    // nobody has looked at" — had to be done by eye before.
    acked: {
      kind: 'enum',
      options: [
        { value: 'true', label: t('history.filter.acked') },
        { value: 'false', label: t('history.filter.unacked') },
      ],
      allLabel: t('history.filter.allAcked'),
    },
    range: {
      kind: 'range',
      // The window becomes `since` in the query, and the missing `readTime` — not a nulled
      // `seconds` — is what stops a client predicate re-applying it against a different clock
      // (Inc.10; `filterPredicate.ts` returns at the accessor, before it reads a length).
      presets: rangePresets(HISTORY_RANGES, t, 'history.range.'),
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
  return {
    severity: f.severity,
    state: f.state,
    phase: f.phase,
    node_q: f.nodeQ ? encodeCondition({ term: f.nodeQ, mode: 'contains', not: false }) : '',
    metric: f.metric ? encodeCondition({ term: f.metric, mode: 'contains', not: false }) : '',
    acked: f.acked,
    range: f.range,
  };
}

export function filtersFromState(
  s: FilterState,
  scope: { nodeId: string; groupId: string },
): HistoryFilters {
  const range = s.range ?? '';
  return {
    // A set column stores its tokens joined; the API takes the same spelling, so this is a
    // pass-through rather than a decode-and-rejoin.
    severity: joined(s.severity, SEVERITIES),
    state: joined(s.state, SEVERITY_ORDER),
    // `phase` maps onto a boolean, so "both ticked" is the same request as "neither" — see
    // `resolvedFor`. Normalising it here keeps `isFiltered` from calling the unfiltered view filtered.
    phase: phaseOf(s.phase),
    // A text column stores an encoded condition; only its term reaches this API (there is no regex
    // and no negated form on either parameter).
    nodeQ: decodeCondition(s.node_q ?? '').term,
    metric: decodeCondition(s.metric ?? '').term,
    acked: ackedOf(s.acked),
    range: (HISTORY_RANGES as readonly string[]).includes(range)
      ? (range as HistoryRange)
      : DEFAULT_FILTERS.range,
    nodeId: scope.nodeId,
    groupId: scope.groupId,
  };
}

/** A token set, re-joined in `order` so the same selection is always the same string. */
function joined(value: string | undefined, order: readonly string[]): string {
  return encodeSet(decodeSet(value ?? ''), order);
}

/** Both phases ticked (or neither) is the unfiltered view; one is a filter. */
function phaseOf(value: string | undefined): HistoryPhase {
  const picked = decodeSet(value ?? '');
  return picked.length === 1 ? (picked[0] as HistoryPhase) : '';
}

/** Same rule for ack: both is the unfiltered view. `''` is "either". */
function ackedOf(value: string | undefined): AckedFilter {
  const picked = decodeSet(value ?? '');
  return picked.length === 1 && (picked[0] === 'true' || picked[0] === 'false')
    ? (picked[0] as AckedFilter)
    : '';
}

/** `''` = either, otherwise the acknowledged state to keep. */
export type AckedFilter = '' | 'true' | 'false';

/** The screen's filter state. `''` means "no filter" for each optional field.
 *
 *  `severity` and `state` are comma-joined token **sets** rather than single values — the API takes
 *  them that way since ADR-053 Inc.4b, and keeping the same spelling here means `queryFor` passes
 *  them through untouched instead of re-encoding a list at the last moment. */
export interface HistoryFilters {
  severity: string;
  state: string;
  phase: HistoryPhase;
  /** Substring of the node's current name (`node_q`). */
  nodeQ: string;
  /** Substring of the metric name (`metric`). */
  metric: string;
  acked: AckedFilter;
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
  nodeQ: '',
  metric: '',
  acked: '',
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
    // `''` means "either", and the two real values are strings in the filter state because that is
    // what a filter cell stores. Only here do they become the boolean the API takes.
    acked: f.acked === '' ? undefined : f.acked === 'true',
    metric: unset(f.metric),
    node_id: unset(f.nodeId),
    node_q: unset(f.nodeQ),
    group_id: unset(f.groupId),
    since: sinceIso(rangeSeconds(f.range), nowMs),
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
export function readFilters(
  params: URLSearchParams,
  severities: readonly Severity[],
  states: readonly NodeState[],
): HistoryFilters {
  return {
    // ⚠️ `readSetParam`, not `readEnumParam`: these carry several tokens now. An older bookmark
    // holding one (`?severity=critical`) still reads correctly — one token is a one-element set.
    severity: readSetParam(params, 'severity', severities),
    state: readSetParam(params, 'state', states),
    phase: readEnumParam(params, 'phase', HISTORY_PHASES, ''),
    nodeQ: readIdParam(params, 'node_q') ?? '',
    metric: readIdParam(params, 'metric') ?? '',
    acked: readEnumParam<AckedFilter>(params, 'acked', ['', 'true', 'false'], ''),
    range: readEnumParam(params, 'range', HISTORY_RANGES, DEFAULT_FILTERS.range),
    nodeId: readIdParam(params, 'node_id') ?? '',
    groupId: readIdParam(params, 'group_id') ?? '',
  };
}

/** Write the filters back, deleting every key whose value is the default so the unfiltered view
 *  has no query string at all. */
export function writeFilters(params: URLSearchParams, f: HistoryFilters): void {
  writeSetParam(params, 'severity', f.severity);
  writeSetParam(params, 'state', f.state);
  writeEnumParam(params, 'phase', f.phase, '');
  writeIdParam(params, 'node_q', f.nodeQ.trim() || null);
  writeIdParam(params, 'metric', f.metric.trim() || null);
  writeEnumParam(params, 'acked', f.acked, '');
  writeEnumParam(params, 'range', f.range, DEFAULT_FILTERS.range);
  writeIdParam(params, 'node_id', f.nodeId || null);
  writeIdParam(params, 'group_id', f.groupId || null);
}
