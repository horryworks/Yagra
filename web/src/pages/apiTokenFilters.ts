// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ API tokens table shows.
//
// Client-side: the token list is bounded by what an admin issued, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, textMatch } from '../lib/filterQuery';
import type { SortState, SortValues } from '../lib/tableSort';
import { tokenState, TOKEN_STATES, type TokenState } from './tokenForm';
import type { ApiTokenSummary } from '../types/api';

/** The states the dropdown offers — the listing's own vocabulary, so a state the badge can show is
 *  always a state the filter can select. */
export const TOKEN_STATE_FILTERS = TOKEN_STATES;

export interface TokenFilters {
  state: TokenState | '';
  /** Free text over the token's name and its owner. */
  q: string;
}

export const DEFAULT_TOKEN_FILTERS: TokenFilters = { state: '', q: '' };

/**
 * Whether one token survives the filter.
 *
 * `now` is a parameter, and one reading is used for a whole render: `tokenState` compares against
 * the expiry, so two rows evaluated a millisecond apart could otherwise disagree about a token
 * lapsing between them — and the filter would then show a row the badge calls something else.
 */
export function matchesToken(row: ApiTokenSummary, f: TokenFilters, now: Date): boolean {
  if (f.state && tokenState(row, now) !== f.state) return false;
  return textMatch(f.q, row.name, row.owner);
}

export function isTokenFiltered(f: TokenFilters): boolean {
  return isFilteredAgainst(f, DEFAULT_TOKEN_FILTERS);
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
