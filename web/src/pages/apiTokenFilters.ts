// SPDX-License-Identifier: AGPL-3.0-only
// Which rows the Settings ▸ API tokens table shows.
//
// Client-side: the token list is bounded by what an admin issued, not by fleet size
// (ui-conventions). In a `.ts` so a test can reach it (testing.md).

import { isFiltered as isFilteredAgainst, textMatch } from '../lib/filterQuery';
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
