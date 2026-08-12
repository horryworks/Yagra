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

import { alertSubject, type HasSubject } from '../lib/alertSubject';
import { isFiltered as isFilteredAgainst } from '../lib/filterQuery';
import { readEnumParam, readIdParam, writeEnumParam, writeIdParam } from '../lib/filterParams';
import type { NodeState, Severity } from '../types/api';

/** The acknowledgement states the screen offers, in the order the dropdown lists them.
 *
 *  Ack is *inbound* state mirrored from the external on-call tool (ADR-015) — Yagra has no ack
 *  action — so this filter answers "has anyone picked this up yet", which is the first question
 *  during triage and the one the read-only pill exists to answer. */
export const ACK_STATES = ['unacked', 'acked'] as const;

/** Those plus `''` for "no filter". Derived, so the dropdown and the URL codec cannot disagree
 *  about which values exist. */
export const ACK_FILTERS = ['', ...ACK_STATES] as const;
export type AckFilter = (typeof ACK_FILTERS)[number];

/** The screen's filter state. `''` means "no filter" for each field. */
export interface ActiveAlertFilters {
  severity: Severity | '';
  state: NodeState | '';
  ack: AckFilter;
  /** Free text over the subject and what fired. */
  q: string;
}

/** The default view: every open alert, worst first. */
export const DEFAULT_FILTERS: ActiveAlertFilters = {
  severity: '',
  state: '',
  ack: '',
  q: '',
};

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
 * Whether one alert survives the filter.
 *
 * ⚠️ **The `q === ''` early return is load-bearing, not a micro-optimization.** `nameOf` is
 * `useEntityNames`' lazy resolver: asking it about an id *enqueues* that id for the next batch
 * request. Calling it for every alert on every render would resolve the whole outage's node names
 * in order to draw thirty rows, which is precisely what that batching exists to avoid. So names are
 * resolved only once an operator has actually typed something, and the default view costs nothing.
 *
 * The free text matches the subject (a node's name **or** its raw UUID, or a pool's name) and the
 * metric that fired. The raw id is included because it is the handle that appears in a deep link
 * and in an API error, so pasting one has to find its row — the same rule as the Credentials list.
 */
export function matchesActiveAlert(
  a: FilterableAlert,
  f: ActiveAlertFilters,
  nameOf: NameOf,
): boolean {
  if (f.severity && a.severity !== f.severity) return false;
  if (f.state && a.state !== f.state) return false;
  if (f.ack === 'acked' && !a.acked) return false;
  if (f.ack === 'unacked' && a.acked) return false;

  const q = f.q.trim().toLowerCase();
  if (q === '') return true;

  const subject = alertSubject(a);
  const haystack =
    subject.kind === 'node' ? [nameOf(subject.nodeId), subject.nodeId] : [subject.name];
  if (a.metric) haystack.push(a.metric);
  return haystack.some((s) => s.toLowerCase().includes(q));
}

/** Whether anything is narrowing the list — which picks the empty state's wording.
 *
 *  Derived from the defaults so a filter added without its clause cannot make the screen claim
 *  there is nothing here while a filter is hiding the rows (`lib/filterQuery.ts`). */
export function isFiltered(f: ActiveAlertFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_FILTERS);
}

/**
 * Read the filters out of the URL.
 *
 * The URL is their only source of truth here: nothing else holds them, so a narrowed triage view
 * survives a reload and can be pasted into a chat during an incident — which is the whole point on
 * a screen two people are looking at at once.
 */
export function readFilters(
  params: URLSearchParams,
  severities: readonly Severity[],
  states: readonly NodeState[],
): ActiveAlertFilters {
  return {
    severity: readEnumParam(params, 'severity', ['', ...severities], ''),
    state: readEnumParam(params, 'state', ['', ...states], ''),
    ack: readEnumParam(params, 'ack', ACK_FILTERS, ''),
    q: readIdParam(params, 'q') ?? '',
  };
}

/** Write the filters back, deleting every key whose value is the default so the unfiltered view
 *  has no query string at all. */
export function writeFilters(params: URLSearchParams, f: ActiveAlertFilters): void {
  writeEnumParam(params, 'severity', f.severity, '');
  writeEnumParam(params, 'state', f.state, '');
  writeEnumParam(params, 'ack', f.ack, '');
  writeIdParam(params, 'q', f.q.trim() || null);
}
