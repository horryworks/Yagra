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
  normalizeSets,
  rangeSecondsIn,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { decodeCondition } from '../lib/filterCondition';
import { sinceIso, unset } from '../lib/filterQuery';
import { rangePresets, type RangeToken } from '../lib/filterPresets';
import { severityLabel, stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { PAGE_SIZE } from './historyCursor';
import { SEVERITIES, type AlertHistoryRow } from '../types/api';
import type { ScopeIds } from '../troubleshoot/findingsQuery';

/** The lifecycle phases the column offers, mapped to the API's `resolved` boolean. `''` — both
 *  ticked, or neither — is the unfiltered view rather than a third phase. */
export const HISTORY_PHASES = ['fired', 'cleared'] as const;
export type HistoryPhase = '' | (typeof HISTORY_PHASES)[number];

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
 *
 * ⚠️ **Declared in the table's column order.** The page derives its filter list from this record
 * rather than from the columns — a display column closes over the resolved node name, so deriving
 * from it would refetch the first page every time a name batch lands, discarding the pages below —
 * and the mobile sheet lists filters in the order it is given.
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
    state: {
      kind: 'enum',
      options: SEVERITY_ORDER.map((s) => ({ value: s, label: stateLabel(s) })),
      allLabel: t('history.filter.allStates'),
    },
    phase: {
      kind: 'enum',
      options: HISTORY_PHASES.map((v) => ({ value: v, label: t(`history.phase.${v}`) })),
      allLabel: t('history.filter.allPhases'),
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
      // `all` rather than a bounded window. `alert_history_cursor_idx` orders the table by exactly
      // the columns the cursor pages on, so an unfiltered first page is an index seek of 100 rows
      // however large the log is — and History showing everything on open is what it has always
      // done. (All findings defaults to 7d for the opposite reason: it has no such index.)
      //
      // ⚠️ This literal is the screen's whole default state now (Inc.10): `defaultFilters(columns)`
      // derives the rest, so there is no hand-written defaults object to forget a key in — and no
      // second URL codec either, since `useFilterParams` reads and writes these keys directly.
      defaultPreset: 'all',
    },
  };
}

/** The columns this module's functions read (ADR-053 Inc.10, 決定 AA). */
export type HistoryColumns = readonly FilterableColumn<AlertHistoryRow>[];

/** Both phases ticked (or neither) is the unfiltered view; exactly one is a filter.
 *
 *  Reading `picked[0]` without re-checking it is safe only because `queryFor` has already run the
 *  state through `normalizeSets`, which drops any token the column does not offer. */
function phaseOf(value: string | undefined): HistoryPhase {
  const picked = decodeSet(value ?? '');
  return picked.length === 1 ? (picked[0] as HistoryPhase) : '';
}

/** Same rule for ack, straight to the boolean the API takes: `undefined` is "either". */
function ackedOf(value: string | undefined): boolean | undefined {
  const picked = decodeSet(value ?? '');
  return picked.length === 1 ? picked[0] === 'true' : undefined;
}

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
 * ⚠️ **`normalizeSets` is what keeps a hand-typed `?severity=bogus` out of that same 400**, and on
 * this screen it does a second job: `phaseOf`/`ackedOf` below read `picked[0]` as a known token,
 * which is only true downstream of it.
 *
 * The scope is a parameter rather than a column because it is not one — the ScopePicker in the
 * action row answers "all / this group / this node", and its two ids ride in the URL beside the
 * columns (`useFilterParams.setFilters(next, also)`).
 *
 * `nowMs` is a parameter so a relative range is testable without faking the clock.
 */
export function queryFor(
  columns: HistoryColumns,
  s: FilterState,
  scope: ScopeIds,
  cursor: { before: string; before_id: string } | null,
  nowMs: number,
) {
  const f = normalizeSets(columns, s);
  return {
    limit: PAGE_SIZE,
    severity: unset(f.severity),
    state: unset(f.state),
    resolved: resolvedFor(phaseOf(f.phase)),
    // Two ticked boxes are strings in the filter state because that is what a filter cell stores.
    // Only here do they become the boolean the API takes — and "both" is `undefined`, not `false`.
    acked: ackedOf(f.acked),
    // The text columns store an encoded condition; only the term reaches this API, which has
    // neither a regex nor a negated form for either parameter.
    metric: unset(decodeCondition(f.metric ?? '').term),
    node_id: unset(scope.nodeId),
    node_q: unset(decodeCondition(f.node_q ?? '').term),
    group_id: unset(scope.groupId),
    since: sinceIso(rangeSecondsIn(columns, f), nowMs),
    before: cursor?.before,
    before_id: cursor?.before_id,
  };
}

/**
 * The scope ids the URL carries beside the columns, and the writer that puts them back.
 *
 * They are the reason this screen keeps its filters in the URL at all: a node page linking to
 * "this node's alert history" needs somewhere to say which node, and nothing else holds that.
 *
 * ⚠️ `writeScope` is meant to be handed to `setFilters(next, also)` — **never called beside a
 * second `setSearchParams`**. Two writes in one handler are both built from this render's snapshot
 * and React batches them, so the second silently discards the first; that is exactly how "clear all
 * filters" once cleared the columns and restored them on the Events page.
 */
export function readScope(params: URLSearchParams): ScopeIds {
  return {
    nodeId: params.get('node_id')?.trim() ?? '',
    groupId: params.get('group_id')?.trim() ?? '',
  };
}

export function writeScope(scope: ScopeIds): (params: URLSearchParams) => void {
  return (params) => {
    for (const [key, value] of [
      ['node_id', scope.nodeId],
      ['group_id', scope.groupId],
    ] as const) {
      // Deleted rather than emptied, the same rule `writeFilterParams` follows: a bare URL is the
      // default view, so a query string always means something is narrowing the list.
      if (value) params.set(key, value);
      else params.delete(key);
    }
  };
}
