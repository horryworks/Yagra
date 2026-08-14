// SPDX-License-Identifier: AGPL-3.0-only
// The Events screens' column filters: the specs, and the query they become (ADR-053 Inc.2).
//
// This is the first screen on the filter row, and it is the one that carries the hard half — its
// list is server-side, so a filter here is not a predicate over rows in the browser but a set of
// query parameters that two different stores have to answer identically. That is why the specs and
// the mapping live in a `.ts` beside the components rather than inside them: Vitest runs
// `environment: 'node'` and never executes a `.tsx` test (testing.md), so anything with a judgement
// in it has to be here to be testable at all.
//
// ⚠️ **Not one column here carries a row accessor, on purpose.** The rows in the browser are one
// keyset page, not the result set, so filtering them locally would narrow the page and leave the
// operator paging through a list whose "load more" fetches rows the local predicate then hides.
//
// That sentence was already here and was only three-quarters true until ADR-053 Inc.10: `readText`
// was the one accessor the type still *required*, so the two text columns carried one each while
// claiming not to. Making it optional is what let the claim and the code agree.

import type { TFunction } from 'i18next';
import type { EventRow } from '../../types/api';
import { EVENT_ACTIONS, EVENT_KINDS } from '../../types/api';
import {
  CUSTOM_RANGE,
  decodeRange,
  decodeSet,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../../lib/columnFilter';
import { decodeCondition, type TextCondition } from '../../lib/filterCondition';
import { localInputToIso } from '../NodeDetail/RangeControl';
import { boundsFor, DEFAULT_EVENT_RANGE, EVENT_RANGES, type EventRange } from './eventRange';

/** The column keys that carry a filter. They are also the URL keys — see `columnFilter.ts` for why
 *  there is no prefix, and `reservedKeyCollisions` for what makes that checkable. */
export const EVENT_FILTER_KEYS = ['kind', 'source', 'message', 'action', 'at'] as const;

/** How a plain term matches on this deployment, from `GET /system-health`.
 *
 *  `undefined` is a real state, not a placeholder: an N-1 core does not report it, and axum drops
 *  query parameters it does not know **silently**, so on that core every filter below would appear
 *  to do nothing. The screens read this to decide whether to say "whole words" or to say nothing. */
export type SearchSemantics = 'prefix' | 'substring' | undefined;

/**
 * The Events column filters.
 *
 * `t` is the caller's translator, so the specs rebuild on a language change — the labels are inside
 * the descriptors, which is what lets the trigger summarize a selection without the component
 * knowing what kind of column it is.
 */
export function eventFilters(
  t: TFunction,
  opts?: { showSource?: boolean; semantics?: SearchSemantics },
): Record<string, ColumnFilterSpec<EventRow>> {
  const semantics = opts?.semantics;
  // Only a log-store deployment needs the warning; on PostgreSQL a term is a substring and there is
  // nothing to explain. `undefined` says nothing rather than guessing — see `SearchSemantics`.
  const containsSemantics = semantics === 'prefix' ? ('prefix' as const) : undefined;
  const specs: Record<string, ColumnFilterSpec<EventRow>> = {
    kind: {
      kind: 'enum',
      options: EVENT_KINDS.map((v) => ({ value: v, label: v })),
      readValue: (r) => r.kind,
      allLabel: t('alerts:events.filters.allKinds'),
      // The facet counts come from `/events/stats?group_by=kind`, and only when the popover opens
      // (ADR-023): five aggregate queries per page load to decorate checkboxes nobody clicked is
      // how the poll loop starves.
      counts: 'server',
    },
    message: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      containsSemantics,
      placeholder: t('alerts:eventLog.cols.message'),
    },
    action: {
      kind: 'enum',
      options: EVENT_ACTIONS.map((v) => ({
        value: v,
        label: t(`alerts:eventLog.action.${v}`),
      })),
      readValue: (r) => r.action,
      allLabel: t('alerts:events.filters.allEvents'),
      counts: 'server',
    },
    at: {
      kind: 'range',
      presets: EVENT_RANGES.map((r) => ({
        value: r,
        label: t(`alerts:events.range.${r}`),
        // Only the client-side predicate reads `seconds`, and Events has no client-side predicate.
        // The real windows come from `boundsFor`, which stays the one place they are computed.
        seconds: null,
      })),
      defaultPreset: DEFAULT_EVENT_RANGE,
      custom: true,
    },
  };
  if (opts?.showSource ?? true) {
    specs.source = {
      kind: 'text',
      // Contains only. There is no `src_regex` on the wire: a source match spans the event's IP and
      // the attributed node's *name*, and the name half has no counterpart in a log store, so a
      // pattern would mean two different things on two deployments (`events::TextCond`).
      modes: ['contains'],
      not: true,
      containsSemantics,
      placeholder: t('alerts:eventLog.cols.source'),
    };
  }
  return specs;
}

/** The specs as the ordered `FilterableColumn` list the pure helpers walk. */
export function eventFilterColumns(
  t: TFunction,
  opts?: { showSource?: boolean; semantics?: SearchSemantics },
): FilterableColumn<EventRow>[] {
  const specs = eventFilters(t, opts);
  return EVENT_FILTER_KEYS.flatMap((k) => (specs[k] ? [{ key: k, filter: specs[k] }] : []));
}

/** Every filter parameter the two Events screens send, as primitives.
 *
 *  Primitives rather than a nested object because `useEventLog` takes them as its dependency list —
 *  an inline object would be a new identity every render and re-fire the reload effect (see that
 *  hook's header). Blank values are `undefined` so the client drops the key entirely. */
export interface EventQuery {
  kind?: string;
  action?: string;
  msg?: string;
  msg_regex?: boolean;
  msg_not?: boolean;
  src?: string;
  src_not?: boolean;
  start?: string;
  end?: string;
}

/**
 * Turn the filter-row state into the query the API takes.
 *
 * `nowMs` is a parameter for the reason `boundsFor` documents: a relative lower bound must be
 * resolved **once, when the range is chosen**, not per request — one recomputed on every "load
 * older" page creeps forward and silently drops rows the keyset cursor was walking towards.
 */
export function eventFilterQuery(state: FilterState, nowMs: number): EventQuery {
  const q: EventQuery = {};

  const kinds = decodeSet(state.kind ?? '');
  if (kinds.length) q.kind = kinds.join(',');
  const actions = decodeSet(state.action ?? '');
  if (actions.length) q.action = actions.join(',');

  const msg = decodeCondition(state.message ?? '');
  if (msg.term) {
    q.msg = msg.term;
    if (msg.mode === 'regex') q.msg_regex = true;
    if (msg.not) q.msg_not = true;
  }
  const src = decodeCondition(state.source ?? '');
  if (src.term) {
    q.src = src.term;
    if (src.not) q.src_not = true;
  }

  // Decoded against the shape rather than the built spec: this function must be callable without a
  // translator, so it cannot ask `eventFilters` for the presets (their labels are localized).
  const range = decodeRange(state.at ?? '', {
    presets: EVENT_RANGES.map((r) => ({ value: r, label: r, seconds: null })),
    defaultPreset: DEFAULT_EVENT_RANGE,
    custom: true,
  });
  const bounds =
    range.preset === CUSTOM_RANGE
      ? { start: localInputToIso(range.from), end: localInputToIso(range.to) }
      : boundsFor(range.preset as EventRange, {}, nowMs);
  if (bounds.start) q.start = bounds.start;
  if (bounds.end) q.end = bounds.end;
  return q;
}

/** A stable key for the current filters, for effect dependencies and for `useEntityNames` batching.
 *  Same shape as `inventoryFilters.ts::inventoryKey`. */
export function eventFilterKey(q: EventQuery): string {
  return JSON.stringify([
    q.kind ?? '',
    q.action ?? '',
    q.msg ?? '',
    q.msg_regex ?? false,
    q.msg_not ?? false,
    q.src ?? '',
    q.src_not ?? false,
    q.start ?? '',
    q.end ?? '',
  ]);
}

/**
 * Which empty state the screen should show.
 *
 * Three, not two, and the third is the whole reason `search_semantics` is reported. On a log-store
 * deployment a plain term is matched from the start of a word, so `%%01POLICY/6/POLICYPERMIT` is
 * found by `POLICY` but not by `PERMIT` — and the operator is looking at those letters on screen
 * while the screen says nothing matched. "Nothing matches these filters" is true and explains none
 * of that.
 */
export type EventEmptyKind = 'unfiltered' | 'filtered' | 'prefixMiss';

export function eventEmptyKind(
  state: FilterState,
  semantics: SearchSemantics,
  anyFiltered: boolean,
): EventEmptyKind {
  if (!anyFiltered) return 'unfiltered';
  if (semantics === 'prefix') {
    // Only a *plain, non-negated* term can miss this way. A regex reaches inside tokens on either
    // store, and a negated term returning nothing means everything matched — a different story.
    const plain = [state.message ?? '', state.source ?? '']
      .map(decodeCondition)
      .some((c) => c.term !== '' && c.mode === 'contains' && !c.not);
    if (plain) return 'prefixMiss';
  }
  return 'filtered';
}

/** Escape a plain term so it matches itself as a regular expression.
 *
 * ⚠️ The character class has to include the backslash itself. A term of `C:\Users` re-asked as
 * `C:\Users` is not the same search — it is a pattern with an undefined escape in it, which the
 * backend refuses at the edge, so the widening would turn "no results" into an error. */
export function escapeRegex(term: string): string {
  return term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * The same query with its plain message term re-asked as a regex, or `null` when that would answer
 * the same question (ADR-053 Inc.2d).
 *
 * The Events page runs this **only after the plain query returned nothing**, and it is the reason a
 * plain term can stay a word-prefix filter: a prefix is answered from the store's dictionary, a
 * match inside a word costs a full scan of the window, and this pays that cost exactly once, in the
 * case where the operator had otherwise reached a dead end.
 *
 * Why it lives in the WebUI and not in the store: `GET /events` returns a bare array, with nowhere
 * to say "I widened your search". Answering a different question than the one asked, silently, is
 * worse than answering narrowly — so the retry belongs where the result can be labelled.
 *
 * `null` for the four cases where widening is not a widening:
 *   - the deployment already matches any substring (PostgreSQL);
 *   - there is no plain term to widen;
 *   - the term is **negated** — an empty result there means *everything* matched, and widening the
 *     thing being excluded would return fewer rows, not more;
 *   - the term is already a regex, which reaches inside words on either store.
 */
export function widenEventQuery(q: EventQuery, semantics: SearchSemantics): EventQuery | null {
  if (semantics !== 'prefix') return null;
  if (!q.msg || q.msg_regex || q.msg_not) return null;
  return { ...q, msg: escapeRegex(q.msg), msg_regex: true };
}

/**
 * Whether to re-ask the current query in its widened form, right now.
 *
 * Pure, and separated from the hook that calls it, because the bug it exists to prevent is not
 * visible on screen: a search that was widened when it did not need to be still shows rows, still
 * highlights them consistently, and still says so in the banner — it is simply answering a broader
 * question than the operator asked, and nothing about the result looks wrong. It shipped that way
 * (ADR-053 Inc.2d, reported 2026-08-13: `POLICY` matches by prefix, and was widened anyway).
 *
 * The load-bearing input is `settled`, and the rule is one line: **an empty list is only evidence
 * of a miss once a fetch for *this* query has come back.** The version that reads `!loading &&
 * rows.length === 0` looks equivalent and is not — see `useEventLog`'s `settled` for the window
 * where those two disagree. Given a previous query that also found nothing, that window is entered
 * on every filter change, so the widening fired for terms that had never been asked about.
 */
export function shouldWiden(s: {
  /** Already widened for this query. The retry is at most one. */
  widened: boolean;
  /** Whether a widened form of this query exists at all (`widenEventQuery` returned non-null). */
  canWiden: boolean;
  /** Whether `rowCount` describes the query currently being asked. */
  settled: boolean;
  rowCount: number;
}): boolean {
  return !s.widened && s.canWiden && s.settled && s.rowCount === 0;
}

/**
 * What the Message column was matched on, for the highlighter — built once per query, never per row.
 *
 * `widened` matters here for the same reason it matters to the query: after the automatic retry the
 * term really was matched inside words, so the marks have to say so (ADR-053 Inc.2d/2e).
 */
export function eventHighlight(
  state: FilterState,
  semantics: SearchSemantics,
  widened: boolean,
): { cond: TextCondition | null; semantics: SearchSemantics; widened: boolean } {
  const cond = decodeCondition(state.message ?? '');
  return { cond: cond.term ? cond : null, semantics, widened };
}
