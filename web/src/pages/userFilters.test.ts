// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Settings ▸ Users filter (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import { ROLES, USER_KINDS, type UserSummary } from '../types/api';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { facetCounts } from '../lib/filterCounts';
import { userColumns, userFilterLabels, USER_STATUSES } from './userFilters';

const t = ((k: string) => k) as unknown as TFunction;
const COLS = userColumns(t);
const DEFAULTS = defaultFilters(COLS);
const f = (over: FilterState): FilterState => ({ ...DEFAULTS, ...over });
const matches = (u: UserSummary, state: FilterState) => buildPredicate(COLS, state, 0)(u);

const user = (over: Partial<UserSummary> = {}): UserSummary => ({
  id: 'u1',
  username: 'alice',
  role: 'admin',
  enabled: true,
  auth_source: 'local',
  created_at: '2026-01-01T00:00:00Z',
  last_login_at: null,
  scope: 'All',
  ...over,
});

describe('the option lists come from the shared enums', () => {
  it('offers every role, most-privileged first', () => {
    // The identity list orders admin → operator → viewer, and the option order is also the URL's
    // token order (`encodeSet` sorts by it), so two people ticking the same boxes get one link.
    const role = COLS.find((c) => c.key === 'role')?.filter;
    expect(role?.kind).toBe('enum');
    if (role?.kind !== 'enum') return;
    expect(role.options.map((o) => o.value)).toEqual([...ROLES].reverse());
  });

  it('offers every account kind, including the ones nobody can create', () => {
    // ⚠️ Deliberately the whole `USER_KINDS`, not the creatable subset `CREATABLE_USER_KINDS` the
    // add-user form uses. An `oidc` or `ldap` account cannot be created here but certainly exists,
    // and a filter that could not name it would hide accounts an admin came looking for.
    const kind = COLS.find((c) => c.key === 'kind')?.filter;
    expect(kind?.kind).toBe('enum');
    if (kind?.kind !== 'enum') return;
    expect(kind.options.map((o) => o.value)).toEqual([...USER_KINDS]);
  });

  it('labels every column, so the bar never shows a raw key', () => {
    const labels = userFilterLabels(t);
    for (const c of COLS) expect(labels[c.key]).toBeTruthy();
  });
});

describe('the predicate', () => {
  it('shows everything when nothing is set', () => {
    expect(matches(user(), DEFAULTS)).toBe(true);
    expect(matches(user({ enabled: false, auth_source: 'oidc' }), DEFAULTS)).toBe(true);
  });

  it('takes several roles at once — the thing the segmented control could not say', () => {
    // "Who can change anything here" is admins *and* operators, and it was unsayable with a
    // one-of-four radio group. It is the question an access review starts from.
    const set = f({ role: 'admin,operator' });
    expect(matches(user({ role: 'admin' }), set)).toBe(true);
    expect(matches(user({ role: 'operator' }), set)).toBe(true);
    expect(matches(user({ role: 'viewer' }), set)).toBe(false);
  });

  it('derives status from the row’s one boolean', () => {
    expect(USER_STATUSES).toEqual(['active', 'disabled']);
    expect(matches(user({ enabled: true }), f({ status: 'active' }))).toBe(true);
    expect(matches(user({ enabled: true }), f({ status: 'disabled' }))).toBe(false);
    expect(matches(user({ enabled: false }), f({ status: 'disabled' }))).toBe(true);
    // Both ticked is the same as neither — never "nothing matches".
    expect(matches(user({ enabled: false }), f({ status: 'active,disabled' }))).toBe(true);
  });

  it('filters by where the account comes from', () => {
    expect(matches(user({ auth_source: 'oidc' }), f({ kind: 'oidc' }))).toBe(true);
    expect(matches(user({ auth_source: 'local' }), f({ kind: 'oidc' }))).toBe(false);
    expect(matches(user({ auth_source: 'service' }), f({ kind: 'service,ldap' }))).toBe(true);
  });

  it('searches the username, case-insensitively, and can exclude', () => {
    expect(matches(user({ username: 'Alice' }), f({ q: 'ali' }))).toBe(true);
    expect(matches(user({ username: 'bob' }), f({ q: 'ali' }))).toBe(false);
    expect(matches(user({ username: 'svc-backup' }), f({ q: '!svc-' }))).toBe(false);
    expect(matches(user({ username: 'alice' }), f({ q: '!svc-' }))).toBe(true);
  });

  it('applies every set filter together', () => {
    const u = user({ role: 'operator', enabled: false });
    expect(matches(u, f({ role: 'operator', status: 'disabled' }))).toBe(true);
    expect(matches(u, f({ role: 'operator', status: 'active' }))).toBe(false);
  });

  it('flips isAnyFiltered for every column', () => {
    expect(isAnyFiltered(COLS, DEFAULTS)).toBe(false);
    for (const x of [
      f({ role: 'admin' }),
      f({ status: 'active' }),
      f({ kind: 'local' }),
      f({ q: 'a' }),
    ]) {
      expect(isAnyFiltered(COLS, x)).toBe(true);
    }
  });
});

describe('facet counts exclude the column’s own filter', () => {
  it('keeps the other roles countable while one role is selected', () => {
    // The rule that makes the control readable: selecting `admin` must not report `viewer: 0`, or
    // the operator is told the thing they might switch to is empty when it is not.
    const rows = [
      user({ id: '1', role: 'admin' }),
      user({ id: '2', role: 'operator' }),
      user({ id: '3', role: 'viewer' }),
    ];
    const counts = facetCounts(rows, COLS, f({ role: 'admin' }), 'role', 0);
    expect(counts).toEqual({ admin: 1, operator: 1, viewer: 1 });
    // …while a filter on a *different* column does narrow the counts.
    const narrowed = facetCounts(rows, COLS, f({ q: 'alice' }), 'role', 0);
    expect(narrowed.admin + narrowed.operator + narrowed.viewer).toBe(3);
    const none = facetCounts(rows, COLS, f({ q: 'zzz' }), 'role', 0);
    expect(none).toEqual({ admin: 0, operator: 0, viewer: 0 });
  });
});
