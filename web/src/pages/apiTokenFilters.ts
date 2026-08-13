// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ API tokens table shows.
//
// Client-side: the token list is bounded by what an admin issued, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).
//
// **ADR-053 Inc.3**: the toolbar's search box + state dropdown became a filter row, so the two
// controls that used to sit above the table now sit under the headers they belong to. The one
// behavioural consequence worth knowing: the search box matched name **or** owner in a single
// field, and the filter row cannot express that — each column filters itself. Two columns is the
// more useful control (an operator can now say "owned by alice" without also matching a token
// *named* alice), but a saved URL from before this shipped carries `q=` and no longer means
// anything, which is why `q` is not a column key here.

import type { TFunction } from 'i18next';
import type { ColumnFilterSpec } from '../lib/columnFilter';
import { clientRangePresets, enumOptions } from '../lib/filterPresets';
import type { SortState, SortValues } from '../lib/tableSort';
import { tokenState, TOKEN_STATES } from './tokenForm';
import { ROLES, TOKEN_SURFACES, type ApiTokenSummary } from '../types/api';

/** The states the dropdown offers — the listing's own vocabulary, so a state the badge can show is
 *  always a state the filter can select. */
export const TOKEN_STATE_FILTERS = TOKEN_STATES;

/**
 * The API tokens filter row, keyed by `Column.key`.
 *
 * `now` is threaded in for the reason the old predicate took it: `tokenState` compares against the
 * expiry, so two rows evaluated a millisecond apart could disagree about a token lapsing between
 * them — and the filter would then hide a row whose badge says otherwise. One reading per render.
 *
 * Columns deliberately left unfilterable: **scope** (its cell is a computed phrase, not a value —
 * "3 groups" is a rendering of a list, and an operator filtering on it would be selecting a label)
 * and **expires** (a *future* window, where the shared past-facing presets would read backwards).
 */
export function tokenFilters(
  t: TFunction,
  now: Date,
): Record<string, ColumnFilterSpec<ApiTokenSummary>> {
  return {
    name: {
      kind: 'text',
      modes: ['contains', 'regex'],
      not: true,
      readText: (r) => [r.name],
      containsSemantics: 'substring',
      placeholder: t('cols.name'),
    },
    surfaces: {
      kind: 'enum',
      options: enumOptions(TOKEN_SURFACES, t, 'surface.'),
      // An array-valued column: a token on both surfaces matches a selection of either.
      readValue: (r) => r.surfaces,
      allLabel: t('cols.surfaces'),
      counts: 'client',
    },
    role: {
      kind: 'enum',
      options: enumOptions(ROLES, t, 'common:role.'),
      readValue: (r) => r.role,
      allLabel: t('cols.role'),
      counts: 'client',
    },
    owner: {
      kind: 'text',
      modes: ['contains'],
      not: true,
      // A service account has no owner. `null` is not a candidate string, so it never matches a
      // term — and `Exclude` therefore keeps it, which is the right reading of "not alice".
      readText: (r) => [r.owner],
      containsSemantics: 'substring',
      placeholder: t('cols.owner'),
    },
    status: {
      kind: 'enum',
      options: enumOptions(TOKEN_STATES, t, 'state.'),
      readValue: (r) => tokenState(r, now),
      allLabel: t('cols.status'),
      counts: 'client',
    },
    created: {
      kind: 'range',
      presets: clientRangePresets(t),
      defaultPreset: 'all',
      readTime: (r) => Date.parse(r.created_at),
    },
    lastUsed: {
      kind: 'range',
      presets: clientRangePresets(t),
      defaultPreset: 'all',
      // A token that has never been used has no instant, so every bounded window excludes it —
      // which is what "used in the last 7 days" has to mean for it to be worth asking.
      readTime: (r) => (r.last_used_at ? Date.parse(r.last_used_at) : null),
    },
  };
}

/**
 * How each sortable column of the tokens table compares.
 *
 * Keyed by `Column.key`, because what a cell *renders* is a `ReactNode` — sorting on rendered
 * output is how "12" ends up after "9", and how a badge sorts by its colour rather than its
 * meaning. Each entry says what the cell means instead.
 *
 * Two are worth their comment:
 *
 * - **status** sorts by severity, not alphabetically. `TOKEN_STATES` is already declared
 *   worst-first, so the index in it is the ranking — an operator sorting this column wants the
 *   revoked and expired tokens together at one end, not `active` before `expired` because `a`
 *   precedes `e`.
 * - **expires** puts "never" last in both directions, via the missing-value rule in `sortRows`. A
 *   token with no expiry is not "expires at the beginning of time"; sorting it as an empty date
 *   would fill the top of the screen with the rows the operator was sorting *away* from.
 *
 * `now` is threaded through for the same reason `matchesToken` takes it: one clock reading per
 * render, so the sort and the badge cannot disagree about a token lapsing between two rows.
 */
export function tokenSortValues(now: Date): SortValues<ApiTokenSummary> {
  return {
    name: (r) => r.name,
    role: (r) => r.role,
    owner: (r) => r.owner ?? null,
    status: (r) => TOKEN_STATES.indexOf(tokenState(r, now)),
    expires: (r) => r.expires_at ?? null,
    created: (r) => r.created_at,
    lastUsed: (r) => r.last_used_at ?? null,
  };
}

/** The tokens table's initial order: newest first, which is what the API already returns. */
export const DEFAULT_TOKEN_SORT: SortState = { by: 'created', dir: 'desc' };
