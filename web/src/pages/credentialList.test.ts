// SPDX-License-Identifier: AGPL-3.0-only
// What the Credentials table shows and in what order.
//
// The whole list is client-side, so these two functions are the entire answer to "why can I not
// find that credential" — and both failure modes are silent: a search that stops matching the id
// column looks like an empty result set, and a sort that inherits the previous column's direction
// looks like a table that is simply ordered oddly.

import { describe, expect, it } from 'vitest';
import {
  nextCredentialSort,
  visibleCredentials,
  type CredentialSort,
} from './credentialList';

const cred = (id: string, name: string, kind: string, used_by = 0) => ({ id, name, kind, used_by });

const rows = [
  cred('11111111-aaaa-0000-0000-000000000001', 'core-ro', 'snmp_v2c', 12),
  cred('22222222-bbbb-0000-0000-000000000002', 'Edge v3', 'snmp_v3', 0),
  cred('33333333-cccc-0000-0000-000000000003', 'meraki-org', 'meraki_api', 3),
];

const byName: CredentialSort = { key: 'name', dir: 1 };

describe('the search box', () => {
  it('shows everything when it is empty or blank', () => {
    expect(visibleCredentials(rows, '', 'all', byName)).toHaveLength(3);
    expect(visibleCredentials(rows, '   ', 'all', byName)).toHaveLength(3);
  });

  it('matches a substring of the name, ignoring case', () => {
    expect(visibleCredentials(rows, 'EDGE', 'all', byName).map((c) => c.name)).toEqual(['Edge v3']);
    expect(visibleCredentials(rows, 'or', 'all', byName).map((c) => c.name)).toEqual([
      'core-ro',
      'meraki-org',
    ]);
  });

  it('matches the credential id too', () => {
    // The id is the handle that turns up in a node's config and in error text; pasting one has to
    // land on its row.
    expect(visibleCredentials(rows, '22222222-BBBB', 'all', byName).map((c) => c.name)).toEqual([
      'Edge v3',
    ]);
  });

  it('trims what was typed', () => {
    expect(visibleCredentials(rows, '  edge  ', 'all', byName).map((c) => c.name)).toEqual([
      'Edge v3',
    ]);
  });

  it('yields nothing when nothing matches', () => {
    expect(visibleCredentials(rows, 'no-such-thing', 'all', byName)).toEqual([]);
  });
});

describe('the type filter', () => {
  it('passes everything through on "all"', () => {
    expect(visibleCredentials(rows, '', 'all', byName)).toHaveLength(3);
  });

  it('keeps only an exact kind match', () => {
    expect(visibleCredentials(rows, '', 'snmp_v3', byName).map((c) => c.name)).toEqual(['Edge v3']);
    // A kind the select does not offer (Meraki keys are created elsewhere) still filters exactly.
    expect(visibleCredentials(rows, '', 'meraki_api', byName).map((c) => c.name)).toEqual([
      'meraki-org',
    ]);
  });

  it('applies together with the search, not instead of it', () => {
    expect(visibleCredentials(rows, 'edge', 'snmp_v2c', byName)).toEqual([]);
    expect(visibleCredentials(rows, 'edge', 'snmp_v3', byName).map((c) => c.name)).toEqual([
      'Edge v3',
    ]);
  });
});

describe('the sort', () => {
  it('orders by name in either direction', () => {
    // Plain `<` on the raw strings, so it is code-unit order: `Edge v3` leads because uppercase
    // sorts before lowercase. Pinned as-is — this is what the page has always done, and changing it
    // to `localeCompare` is a visible reordering, not a tidy-up.
    expect(visibleCredentials(rows, '', 'all', { key: 'name', dir: 1 }).map((c) => c.name)).toEqual([
      'Edge v3',
      'core-ro',
      'meraki-org',
    ]);
    expect(
      visibleCredentials(rows, '', 'all', { key: 'name', dir: -1 }).map((c) => c.name),
    ).toEqual(['meraki-org', 'core-ro', 'Edge v3']);
  });

  it('orders by usage count numerically, not as text', () => {
    // `12` must not sort before `3` the way string comparison would.
    expect(
      visibleCredentials(rows, '', 'all', { key: 'used_by', dir: 1 }).map((c) => c.used_by),
    ).toEqual([0, 3, 12]);
    expect(
      visibleCredentials(rows, '', 'all', { key: 'used_by', dir: -1 }).map((c) => c.used_by),
    ).toEqual([12, 3, 0]);
  });

  it('leaves ties in their existing order', () => {
    const tied = [cred('a', 'a', 'snmp_v2c', 5), cred('b', 'b', 'snmp_v2c', 5)];
    expect(
      visibleCredentials(tied, '', 'all', { key: 'used_by', dir: 1 }).map((c) => c.name),
    ).toEqual(['a', 'b']);
    expect(
      visibleCredentials(tied, '', 'all', { key: 'used_by', dir: -1 }).map((c) => c.name),
    ).toEqual(['a', 'b']);
  });

  it("never reorders the caller's array", () => {
    // The source is React state; sorting it in place would mutate the store and skip a render.
    const source = [...rows];
    visibleCredentials(source, '', 'all', { key: 'used_by', dir: -1 });
    expect(source.map((c) => c.name)).toEqual(rows.map((c) => c.name));
  });
});

describe('clicking a column header', () => {
  it('flips the direction of the column already sorted', () => {
    expect(nextCredentialSort({ key: 'name', dir: 1 }, 'name')).toEqual({ key: 'name', dir: -1 });
    expect(nextCredentialSort({ key: 'name', dir: -1 }, 'name')).toEqual({ key: 'name', dir: 1 });
  });

  it('starts a different column ascending', () => {
    // Carrying the previous column's direction over would show the new column backwards on its
    // first click, which reads as a broken sort.
    expect(nextCredentialSort({ key: 'name', dir: -1 }, 'used_by')).toEqual({
      key: 'used_by',
      dir: 1,
    });
  });
});
