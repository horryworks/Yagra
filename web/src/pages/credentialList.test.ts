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
  CREDENTIAL_KIND_LABEL_KEYS,
  DEFAULT_CREDENTIAL_SORT,
  credentialFilters,
  credentialSortValues,
  kindLabel,
  usageLabel,
} from './credentialList';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import type { TFunction } from 'i18next';
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

describe('credential kind and usage labels', () => {
  const t = ((key: string, opts?: Record<string, unknown>) =>
    opts && 'count' in opts ? `${key}=${opts.count}` : key) as unknown as TFunction;

  it('labels every kind this build knows', () => {
    expect(kindLabel('snmp_v2c', t)).toBe('cred.kind.snmp_v2c');
    expect(kindLabel('meraki_api', t)).toBe('cred.kind.meraki_api');
  });

  it('shows the raw token for a kind a newer core stored', () => {
    // Honest beats blank: an empty cell reads as a broken row, `snmp_v4` reads as "upgrade me".
    expect(kindLabel('snmp_v4', t)).toBe('snmp_v4');
    expect(kindLabel('', t)).toBe('');
  });

  it('gives "unused" its own phrase rather than counting to zero', () => {
    // Unused is the state an operator looks for when deciding what may be deleted.
    expect(usageLabel(0, t)).toBe('cred.usage.unused');
    expect(usageLabel(1, t)).toBe('cred.usage.count=1');
    expect(usageLabel(12, t)).toBe('cred.usage.count=12');
  });
});

describe('the credential-kind maps are two halves of one thing', () => {
  // The labels live here and the icons live in `CredentialsPage.tsx`, because an icon is a
  // component and this module is loaded by a test in a node environment. Nothing but this makes
  // them agree — a kind added to one and not the other renders with a key for a label, or with the
  // generic key icon, and both look plausible.
  //
  // Read as TEXT rather than imported: importing the page would pull React and every modal it
  // renders into a node-environment test, to check a five-line object literal.
  const src = readFileSync(join(__dirname, 'CredentialsPage.tsx'), 'utf8');

  it('finds the map it is supposed to be reading', () => {
    // Without this, a renamed const makes every assertion below vacuously true.
    expect(src).toContain('const KIND_ICONS: Record<string, ComponentType> = {');
  });

  it('names exactly the same kinds on both sides', () => {
    const block = src.slice(src.indexOf('const KIND_ICONS'));
    const body = block.slice(block.indexOf('{'), block.indexOf('};') + 1);
    const iconKinds = [...body.matchAll(/^\s{2}(\w+):/gm)].map((m) => m[1]).sort();
    expect(iconKinds.length).toBeGreaterThan(3);
    expect(iconKinds).toEqual(Object.keys(CREDENTIAL_KIND_LABEL_KEYS).sort());
  });
});
