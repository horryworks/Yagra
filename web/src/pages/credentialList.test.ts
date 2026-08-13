// SPDX-License-Identifier: AGPL-3.0-only
// What the Credentials table shows and in what order.
//
// The whole list is client-side, so this module is the entire answer to "why can I not find that
// credential" — and the failure modes are silent: a filter that stops matching the id column looks
// like an empty result set, and a sort that inherits the previous column's direction looks like a
// table that is simply ordered oddly.
//
// ADR-053 Inc.5 replaced `visibleCredentials` (search + kind filter + sort in one function) with a
// filter row over the shared predicate, and the sort with `lib/tableSort.ts`. The properties below
// are the ones that survived the move, plus the two the move fixed.

import { describe, expect, it } from 'vitest';
import type { CredentialSummary } from '../types/api';
import {
  credentialFilters,
  credentialSortValues,
  DEFAULT_CREDENTIAL_SORT,
} from './credentialList';
import { specColumns, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { encodeCondition } from '../lib/filterCondition';
import { nextSort, sortRows } from '../lib/tableSort';

const cred = (id: string, name: string, kind: string, used_by = 0) =>
  ({ id, name, kind, used_by }) as CredentialSummary;

const rows = [
  cred('11111111-aaaa-0000-0000-000000000001', 'core-ro', 'snmp_v2c', 12),
  cred('22222222-bbbb-0000-0000-000000000002', 'Edge v3', 'snmp_v3', 0),
  cred('33333333-cccc-0000-0000-000000000003', 'meraki-org', 'meraki_api', 3),
];

const t = ((k: string) => k) as unknown as Parameters<typeof credentialFilters>[0];
const kinds = [...new Set(rows.map((c) => c.kind))].sort();
const specs = credentialFilters(t, kinds, (k) => k);
const term = (s: string) => encodeCondition({ term: s, mode: 'contains', not: false });

function shown(state: FilterState): string[] {
  const keep = buildPredicate(specColumns(specs as Record<string, ColumnFilterSpec<CredentialSummary>>), state, 0);
  return rows.filter(keep).map((c) => c.name);
}

describe('the filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(shown({})).toHaveLength(3);
  });

  it('matches the name case-insensitively', () => {
    expect(shown({ name: term('EDGE') })).toEqual(['Edge v3']);
    expect(shown({ name: term('or') })).toEqual(['core-ro', 'meraki-org']);
    expect(shown({ name: term('no-such-thing') })).toEqual([]);
  });

  it('matches the credential id under its own column, not the name box', () => {
    // The id is the handle that appears in a node's config and in an error message, so pasting one
    // has to find its row. It used to share the name's search box, which meant a term could match
    // either and the operator could not say which they meant.
    expect(shown({ id: term('22222222-BBBB') })).toEqual(['Edge v3']);
    expect(shown({ name: term('22222222-bbbb') })).toEqual([]);
  });

  it('offers every kind that is on screen, including ones nobody can create', () => {
    // 🚨 The old dropdown hardcoded three kinds, so a `meraki_api` row — created by the integration
    // rather than by an operator — sat in the table and could not be filtered for at all.
    const type = specs.type;
    expect(type.kind === 'enum' && type.options.map((o) => o.value)).toEqual([
      'meraki_api',
      'snmp_v2c',
      'snmp_v3',
    ]);
    expect(shown({ type: 'meraki_api' })).toEqual(['meraki-org']);
  });

  it('selects several kinds at once, which the old dropdown could not', () => {
    expect(shown({ type: 'snmp_v2c,snmp_v3' })).toEqual(['core-ro', 'Edge v3']);
  });

  it('ANDs the columns', () => {
    expect(shown({ type: 'snmp_v2c', name: term('core') })).toEqual(['core-ro']);
    expect(shown({ type: 'snmp_v3', name: term('core') })).toEqual([]);
  });
});

describe('the sort', () => {
  const values = credentialSortValues<CredentialSummary>();

  it('starts on the name, ascending', () => {
    expect(DEFAULT_CREDENTIAL_SORT).toEqual({ by: 'name', dir: 'asc' });
  });

  it('orders by usage numerically, not as text', () => {
    // A string sort puts 12 before 3. `used_by` is a count, so the accessor must return a number.
    const desc = sortRows(rows, { by: 'used_by', dir: 'desc' }, values).map((c) => c.used_by);
    expect(desc).toEqual([12, 3, 0]);
  });

  it('starts a new column ascending rather than inheriting the previous direction', () => {
    // Inheriting `desc` from an unrelated column reads as the click not having worked.
    const afterDesc = nextSort({ by: 'name', dir: 'asc' }, 'name');
    expect(afterDesc).toEqual({ by: 'name', dir: 'desc' });
    expect(nextSort(afterDesc, 'used_by')).toEqual({ by: 'used_by', dir: 'asc' });
  });
});
