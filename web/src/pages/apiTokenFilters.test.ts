// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ API tokens filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ApiTokenSummary } from '../types/api';
import { TOKEN_STATES } from './tokenForm';
import {
  DEFAULT_TOKEN_SORT,
  TOKEN_STATE_FILTERS,
  tokenFilters,
  tokenSortValues,
} from './apiTokenFilters';
import {
  defaultFilters,
  isAnyFiltered,
  reservedKeyCollisions,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { applyFilters, matchesFilters } from '../lib/filterPredicate';
import { facetCounts } from '../lib/filterCounts';

const NOW = new Date('2026-08-13T12:00:00.000Z');
const NOW_MS = NOW.getTime();

/** A translator stand-in that returns the key, so a missing label shows up as its key. */
const t = ((k: string) => k) as unknown as Parameters<typeof tokenFilters>[0];

const COLUMNS: FilterableColumn<ApiTokenSummary>[] = Object.entries(tokenFilters(t, NOW)).map(
  ([key, filter]) => ({ key, filter }),
);
const DEFAULTS = defaultFilters(COLUMNS);
const f = (over: Record<string, string>): FilterState => ({ ...DEFAULTS, ...over });

const token = (over: Partial<ApiTokenSummary> = {}): ApiTokenSummary => ({
  id: 't1',
  name: 'grafana reader',
  role: 'viewer',
  surfaces: ['mcp'],
  scope: 'All',
  owner: 'alice',
  owner_active: true,
  owner_last_login_at: null,
  created_at: '2026-01-01T00:00:00.000Z',
  created_by: 'admin',
  expires_at: null,
  last_used_at: null,
  revoked_at: null,
  ...over,
});

describe('the token state vocabulary', () => {
  it("is the listing's own, not a second list", () => {
    // A state the badge can render but the dropdown cannot select would be a row an operator can
    // see and not filter for.
    expect(TOKEN_STATE_FILTERS).toBe(TOKEN_STATES);
  });

  it('uses column keys that do not collide with the page own query params', () => {
    expect(reservedKeyCollisions(COLUMNS)).toEqual([]);
  });

  it('starts unfiltered, so the empty state reads "no tokens" rather than "no matches"', () => {
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
    // ⚠️ Every range column defaults to all time. A client-side list holds every row already, so a
    // narrowed default would hide tokens nobody asked to hide — the opposite of Events, where the
    // bounded default is a performance contract.
    expect(DEFAULTS.created).toBe('all');
    expect(DEFAULTS.lastUsed).toBe('all');
  });
});

describe('the token filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesFilters(token(), COLUMNS, DEFAULTS, NOW_MS)).toBe(true);
  });

  it('filters on the same state the badge shows, including every dead reason', () => {
    // Each of these means the token is refused, and an admin looking for "why does this 401" needs
    // to be able to select the actual reason rather than a single "not active".
    const is = (row: ApiTokenSummary, state: string) =>
      matchesFilters(row, COLUMNS, f({ status: state }), NOW_MS);
    expect(is(token(), 'active')).toBe(true);
    expect(is(token({ revoked_at: '2026-02-01T00:00:00.000Z' }), 'revoked')).toBe(true);
    expect(is(token({ expires_at: '2026-02-01T00:00:00.000Z' }), 'expired')).toBe(true);
    expect(is(token({ owner: null }), 'no-owner')).toBe(true);
    expect(is(token({ owner_active: false }), 'owner-disabled')).toBe(true);
    expect(is(token(), 'revoked')).toBe(false);
  });

  it('selects several states at once, which the old single-choice dropdown could not', () => {
    const rows = [
      token({ id: 'a' }),
      token({ id: 'b', revoked_at: '2026-02-01T00:00:00.000Z' }),
      token({ id: 'c', expires_at: '2026-02-01T00:00:00.000Z' }),
    ];
    const shown = applyFilters(rows, COLUMNS, f({ status: 'revoked,expired' }), NOW_MS);
    expect(shown.map((r) => r.id)).toEqual(['b', 'c']);
  });

  it('matches an array-valued column when any of the row values is selected', () => {
    // A token on both surfaces is one row, and selecting either surface has to keep it.
    const both = token({ surfaces: ['mcp', 'rest'] });
    expect(matchesFilters(both, COLUMNS, f({ surfaces: 'rest' }), NOW_MS)).toBe(true);
    expect(matchesFilters(token({ surfaces: ['mcp'] }), COLUMNS, f({ surfaces: 'rest' }), NOW_MS)).toBe(
      false,
    );
  });

  it('separates the name from the owner, which the single search box could not', () => {
    // The toolbar's one box matched either. Two columns is the more useful control: "owned by
    // alice" no longer also matches a token *called* alice.
    const named = token({ name: 'alice-export', owner: 'bob' });
    expect(matchesFilters(named, COLUMNS, f({ owner: 'alice' }), NOW_MS)).toBe(false);
    expect(matchesFilters(named, COLUMNS, f({ name: 'alice' }), NOW_MS)).toBe(true);
  });

  it('survives an owner-less token instead of crashing on the null', () => {
    expect(matchesFilters(token({ owner: null }), COLUMNS, f({ name: 'grafana' }), NOW_MS)).toBe(true);
    expect(matchesFilters(token({ owner: null }), COLUMNS, f({ owner: 'alice' }), NOW_MS)).toBe(false);
    // …and `Exclude alice` keeps it, because a token with no owner is not owned by alice.
    expect(matchesFilters(token({ owner: null }), COLUMNS, f({ owner: '!alice' }), NOW_MS)).toBe(true);
  });

  it('excludes a never-used token from every bounded "last used" window', () => {
    const never = token({ last_used_at: null });
    expect(matchesFilters(never, COLUMNS, f({ lastUsed: '7d' }), NOW_MS)).toBe(false);
    expect(matchesFilters(never, COLUMNS, DEFAULTS, NOW_MS)).toBe(true);
    const recent = token({ last_used_at: '2026-08-12T12:00:00.000Z' });
    expect(matchesFilters(recent, COLUMNS, f({ lastUsed: '7d' }), NOW_MS)).toBe(true);
    expect(matchesFilters(recent, COLUMNS, f({ lastUsed: '24h' }), NOW_MS)).toBe(true);
  });

  it('counts a facet over the rows that pass the OTHER filters, not the visible ones', () => {
    // ⚠️ The rule that makes an autofilter readable. With `mcp` selected, the `rest` count must
    // still say how many tokens the operator would get by switching — not zero.
    const rows = [
      token({ id: 'a', surfaces: ['mcp'] }),
      token({ id: 'b', surfaces: ['rest'] }),
      token({ id: 'c', surfaces: ['rest'], role: 'admin' }),
    ];
    const counts = facetCounts(rows, COLUMNS, f({ surfaces: 'mcp' }), 'surfaces', NOW_MS);
    expect(counts).toEqual({ mcp: 1, rest: 2 });
    // A different column's filter DOES narrow the counts — that is the half that must not be lost.
    const byRole = facetCounts(rows, COLUMNS, f({ role: 'admin' }), 'surfaces', NOW_MS);
    expect(byRole).toEqual({ mcp: 0, rest: 1 });
  });

  it('flips isAnyFiltered for every column', () => {
    for (const key of Object.keys(DEFAULTS)) {
      const value = key === 'created' || key === 'lastUsed' ? '7d' : 'x';
      expect(isAnyFiltered(COLUMNS, f({ [key]: value }))).toBe(true);
    }
  });
});

describe('tokenSortValues', () => {
  const v = tokenSortValues(NOW);

  it('ranks the status column by severity, not alphabetically', () => {
    // An operator sorting Status wants the tokens that do not work at one end. Alphabetically,
    // `active` would come before `expired` because `a` precedes `e`.
    const revoked = Number(v.status(token({ revoked_at: '2026-01-01T00:00:00.000Z' })));
    const active = Number(v.status(token()));
    expect(revoked).toBeLessThan(active);
  });

  it('reports a token with no expiry as missing, so it sorts last either way', () => {
    // Not "expires at the beginning of time": sorting a never-expiring token as an empty date
    // would fill the top of the screen with the rows being sorted away from.
    expect(v.expires(token({ expires_at: null }))).toBeNull();
    expect(v.expires(token({ expires_at: '2026-09-01T00:00:00.000Z' }))).toBe(
      '2026-09-01T00:00:00.000Z',
    );
  });

  it('reports a never-used token as missing rather than as an empty string', () => {
    expect(v.lastUsed(token({ last_used_at: null }))).toBeNull();
    expect(v.owner(token({ owner: null }))).toBeNull();
  });

  it('has a comparator for every column the page marks sortable', () => {
    // A column with the affordance and no comparator is a header that does nothing when clicked —
    // `sortRows` returns the rows untouched, which reads as the table being stuck.
    expect(Object.keys(v).sort()).toEqual(
      ['created', 'expires', 'lastUsed', 'name', 'owner', 'role', 'status'].sort(),
    );
  });

  it('starts newest-first, matching what the API already returns', () => {
    expect(DEFAULT_TOKEN_SORT).toEqual({ by: 'created', dir: 'desc' });
  });
});
