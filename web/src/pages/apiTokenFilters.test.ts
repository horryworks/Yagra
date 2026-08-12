// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ API tokens filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { ApiTokenSummary } from '../types/api';
import { TOKEN_STATES } from './tokenForm';
import {
  DEFAULT_TOKEN_FILTERS,
  isTokenFiltered,
  matchesToken,
  TOKEN_STATE_FILTERS,
  type TokenFilters,
} from './apiTokenFilters';

const NOW = new Date('2026-08-13T12:00:00.000Z');

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

const f = (over: Partial<TokenFilters>): TokenFilters => ({ ...DEFAULT_TOKEN_FILTERS, ...over });

describe('the token state vocabulary', () => {
  it('is the listing\'s own, not a second list', () => {
    // A state the badge can render but the dropdown cannot select would be a row an operator can
    // see and not filter for.
    expect(TOKEN_STATE_FILTERS).toBe(TOKEN_STATES);
  });
});

describe('matchesToken', () => {
  it('shows everything when nothing is set', () => {
    expect(matchesToken(token(), DEFAULT_TOKEN_FILTERS, NOW)).toBe(true);
  });

  it('filters on the same state the badge shows, including every dead reason', () => {
    // Each of these means the token is refused, and an admin looking for "why does this 401" needs
    // to be able to select the actual reason rather than a single "not active".
    expect(matchesToken(token(), f({ state: 'active' }), NOW)).toBe(true);
    expect(matchesToken(token({ revoked_at: '2026-02-01T00:00:00.000Z' }), f({ state: 'revoked' }), NOW)).toBe(true);
    expect(matchesToken(token({ expires_at: '2026-02-01T00:00:00.000Z' }), f({ state: 'expired' }), NOW)).toBe(true);
    expect(matchesToken(token({ owner: null }), f({ state: 'no-owner' }), NOW)).toBe(true);
    expect(matchesToken(token({ owner_active: false }), f({ state: 'owner-disabled' }), NOW)).toBe(true);
    expect(matchesToken(token(), f({ state: 'revoked' }), NOW)).toBe(false);
  });

  it('searches the name and the owner', () => {
    expect(matchesToken(token(), f({ q: 'GRAFANA' }), NOW)).toBe(true);
    expect(matchesToken(token(), f({ q: 'alice' }), NOW)).toBe(true);
    expect(matchesToken(token(), f({ q: 'bob' }), NOW)).toBe(false);
  });

  it('survives an owner-less token instead of crashing on the null', () => {
    expect(matchesToken(token({ owner: null }), f({ q: 'grafana' }), NOW)).toBe(true);
    expect(matchesToken(token({ owner: null }), f({ q: 'alice' }), NOW)).toBe(false);
  });

  it('flips isFiltered for every field', () => {
    expect(isTokenFiltered(DEFAULT_TOKEN_FILTERS)).toBe(false);
    expect(isTokenFiltered(f({ state: 'revoked' }))).toBe(true);
    expect(isTokenFiltered(f({ q: 'x' }))).toBe(true);
  });
});
