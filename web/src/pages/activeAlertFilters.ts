// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Active — which alerts the triage list shows, as pure functions.
//
// Here rather than in the page for the usual reason: Vitest runs `environment: 'node'` with
// `include: ['src/**/*.test.ts']`, so a test written beside a `.tsx` is a file nothing runs
// (testing.md). The page keeps the layout; every decision about what an operator sees is below.
//
// ⚠️ **This filters in the browser, and that is an exception, not a relaxation of the rule.**
// `ui-conventions.md` classes alert lists with the scale-aware ones that must filter server-side.
// The reason this one may not is specific: the active-alert set is already whole in the browser —
// `useAlertStream` maintains it over SSE and `useAlertStore` holds every open alert — so there is
// no page boundary for a filter to hide older matches behind, which is the failure the server-side
// rule exists to prevent (and which Alerts ▸ History and Settings ▸ Audit actually had). Moving it
// server-side would mean giving the stream a subscription filter — an SSE design change, ADR
// territory — not adding a query parameter.
//
// **ADR-053 Inc.6 moved the controls into a `FilterBar` and the logic onto the shared predicate.**
// `AlertRows` draws a run of spans with no header row, so there is nowhere to hang a filter row
// (decision E). What changed for the operator is not the layout: every dimension became a *set*, so
// "critical and warning" and "unacked, on either of two states" are sayable for the first time, and
// the free text gained the NOT and regex modes every other list has.

import { alertSubject, type HasSubject } from '../lib/alertSubject';
import {
  normalizeSets,
  readFilterParams,
  specColumns,
  writeFilterParams,
  TEXT_MODES,
  type ColumnFilterSpec,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { severityLabel, stateLabel } from '../lib/format';
import type { TFunction } from 'i18next';
import type { NodeState, Severity } from '../types/api';

/** The acknowledgement states the screen offers, in the order the list shows them.
 *
 *  Ack is *inbound* state mirrored from the external on-call tool (ADR-015) — Yagra has no ack
 *  action — so this filter answers "has anyone picked this up yet", which is the first question
 *  during triage and the one the read-only pill exists to answer. */
export const ACK_STATES = ['unacked', 'acked'] as const;
export type AckState = (typeof ACK_STATES)[number];

/**
 * The fields the predicate reads.
 *
 * Structural rather than `Alert` so a test can build a row without the whole `ActiveAlertView`,
 * and so the predicate stays usable against the history row shape if this filter is ever wanted
 * there. `acked` is `unknown` because only its presence is a question here — who acked it and from
 * which tool is the row's tooltip, not a filter.
 */
export interface FilterableAlert extends HasSubject {
  severity: Severity;
  state: NodeState;
  metric?: string | null;
  acked?: unknown;
}

/** Resolve a node id to its display name; the raw id when it has not resolved yet. */
export type NameOf = (id: string) => string;

/**
 * The screen's filter columns.
 *
 * ⚠️ **`nameOf` is a parameter, and the free-text column's `readText` is the only thing that calls
 * it.** That resolver is `useEntityNames`' lazy one: asking it about an id *enqueues* that id for
 * the next batch request, so resolving every alert's node name in order to draw thirty rows is
 * precisely what the batching exists to avoid. `buildPredicate` compiles a column that is not
 * narrowing to `null` and therefore never reads its row — so with the search box empty, nothing is
 * asked and the default view costs nothing. That used to be an explicit `if (q === '') return true`
 * guard in the hand-written predicate; it is now a property of the shared one, which is the whole
 * reason this screen could move onto it.
 *
 * The text matches the subject (a node's name **or** its raw UUID, or a pool's name) and the metric
 * that fired. The raw id is included because it is the handle that appears in a deep link and in an
 * API error, so pasting one has to find its row — the same rule as the Credentials list.
 */
export function activeAlertFilters(
  t: TFunction,
  nameOf: NameOf,
  severities: readonly Severity[],
  states: readonly NodeState[],
): Record<string, ColumnFilterSpec<FilterableAlert>> {
  return {
    severity: {
      kind: 'enum',
      options: severities.map((s) => ({ value: s, label: severityLabel(s) })),
      readValue: (a) => a.severity,
      allLabel: t('history.filter.allSeverities'),
      counts: 'client',
    },
    state: {
      kind: 'enum',
      options: states.map((s) => ({ value: s, label: stateLabel(s) })),
      readValue: (a) => a.state,
      allLabel: t('history.filter.allStates'),
      counts: 'client',
    },
    ack: {
      kind: 'enum',
      options: ACK_STATES.map((v) => ({ value: v, label: t(`active.ack.${v}`) })),
      // Derived from presence, not stored: the row carries who acked it and from which tool, and
      // only whether it is there is a filter.
      readValue: (a) => (a.acked ? 'acked' : 'unacked'),
      allLabel: t('active.ack.all'),
      counts: 'client',
      // Said HERE because this is where someone looking for the ack action arrives (ADR-055 R6).
      // Yagra has no Ack button by decision (ADR-015); the state arrives inbound from the on-call
      // tool. That used to be a parenthetical under the page title, and the person who wrote the
      // decision still had to ask where the button was — so a filter offering `acked` with no
      // on-screen path to it is exactly the case the rule is about.
      hint: t('active.ackedHint'),
    },
    q: {
      kind: 'text',
      // Both modes and NOT: the whole set is in the browser, so a regex costs a pass over a few
      // thousand rows rather than a query, and "everything except the link-flap noise" is the
      // triage question this screen exists for.
      modes: TEXT_MODES,
      not: true,
      readText: (a) => {
        const subject = alertSubject(a);
        return subject.kind === 'node'
          ? [nameOf(subject.nodeId), subject.nodeId, a.metric]
          : [subject.name, a.metric];
      },
      containsSemantics: 'substring',
      placeholder: t('active.searchPlaceholder'),
    },
  };
}

/** Column keys paired with their specs — what the bar, the sheet and the predicate all walk. */
export function activeAlertColumns(
  t: TFunction,
  nameOf: NameOf,
  severities: readonly Severity[],
  states: readonly NodeState[],
): FilterableColumn<FilterableAlert>[] {
  return specColumns(activeAlertFilters(t, nameOf, severities, states));
}

/** Plain-text names for the bar and the mobile sheet, keyed the same way. */
export function activeAlertLabels(t: TFunction): Record<string, string> {
  return {
    severity: t('history.cols.severity'),
    state: t('history.cols.state'),
    ack: t('active.ack.label'),
    q: t('active.searchLabel'),
  };
}

/**
 * The row predicate, built once per `(state, nameOf)` pair.
 *
 * A **factory** rather than a `(row, nameOf) => boolean`, because `AlertRows` applies it per row: a
 * regex compiled per row would be compiled once per alert in an outage. `nowMs` is unused — no
 * column here is a range — but it is passed for the shared signature rather than special-cased.
 */
export function alertPredicate(
  columns: readonly FilterableColumn<FilterableAlert>[],
  state: FilterState,
): (a: FilterableAlert) => boolean {
  return buildPredicate(columns, state, 0);
}

/**
 * Read the filters out of the URL.
 *
 * The URL is their only source of truth here: nothing else holds them, so a narrowed triage view
 * survives a reload and can be pasted into a chat during an incident — which is the whole point on
 * a screen two people are looking at at once.
 *
 * ⚠️ The keys stayed `severity` / `state` / `ack` / `q`, so links taken before the multi-value
 * change still open the same view — a single token is a one-element set. Nothing had to be renamed
 * because these were already the column keys.
 */
export function readFilters(
  columns: readonly FilterableColumn<FilterableAlert>[],
  params: URLSearchParams,
): FilterState {
  // The set re-encoding this used to spell out inline is `normalizeSets` now (Inc.10, 決定 AA) —
  // it was the shape the four server-side screens needed, and they needed it for a sharper reason:
  // an unknown token here just fails to match a row, but there it reaches the API and 400s.
  return normalizeSets(columns, readFilterParams(columns, params));
}

/** Write the filters back, deleting every key whose value is the default so the unfiltered view
 *  has no query string at all. */
export function writeFilters(
  columns: readonly FilterableColumn<FilterableAlert>[],
  params: URLSearchParams,
  next: FilterState,
): void {
  writeFilterParams(columns, params, next);
}
