// SPDX-License-Identifier: AGPL-3.0-only
// The group→role mapping editor's rules.
//
// These tests used to live in `ldapConfigForm.test.ts`, because the LDAP card was the second
// consumer of this module. They were moved here unchanged so the coverage sits next to the file it
// covers — `roleMapForm.ts` is shared by the OIDC provider dialog too, and a reader checking
// whether it is tested should not have to know which caller happened to be written second.

import { describe, expect, it } from 'vitest';

import { addRoleMapRow, asRole, fromRoleMapRows, toRoleMapRows } from './roleMapForm';
import { ROLES } from '../types/api';

const rows = (pairs: [string, 'viewer' | 'operator' | 'admin'][]) =>
  pairs.map(([group, role], key) => ({ key, group, role }));

describe('the role-map rows', () => {
  it('round-trip a stored mapping', () => {
    const map = { NetOps: 'admin', NOC: 'operator' };
    expect(fromRoleMapRows(toRoleMapRows(map))).toEqual(map);
  });

  // The source is a JSON object whose key order is an accident of serialization; an unsorted list
  // would appear to reshuffle itself between visits.
  it('present a stable order', () => {
    expect(toRoleMapRows({ b: 'viewer', a: 'admin' }).map((r) => r.group)).toEqual(['a', 'b']);
  });

  it('drop a blank group and trim the rest', () => {
    expect(fromRoleMapRows(rows([['  ', 'admin'], [' NetOps ', 'operator']]))).toEqual({
      NetOps: 'operator',
    });
  });

  it('fall back to viewer for a role this build does not know', () => {
    // The API types `role_map` values as bare strings, so a row written by a newer build must not
    // silently widen to something more privileged.
    expect(toRoleMapRows({ NetOps: 'superuser' })[0].role).toBe('viewer');
  });

  // Nothing stops an operator typing the same group into two rows, and the object being built can
  // only hold one. Last-wins is the documented choice because it matches the row they edited most
  // recently — pinned here so a refactor to `??=`/first-wins is a failing test rather than a silent
  // change to which of two visible rows takes effect.
  it('let a later row win a duplicate group', () => {
    expect(
      fromRoleMapRows(
        rows([
          ['NetOps', 'viewer'],
          ['NetOps', 'admin'],
        ]),
      ),
    ).toEqual({ NetOps: 'admin' });
  });

  it('give a new row a key that cannot collide', () => {
    const start = toRoleMapRows({ a: 'viewer' });
    const next = addRoleMapRow(start);
    expect(new Set(next.map((r) => r.key)).size).toBe(next.length);
    expect(next[next.length - 1].group).toBe('');
  });
});

describe('asRole', () => {
  it('accepts the roles this build knows and refuses everything else', () => {
    // The value comes from an IdP's group mapping, so it is external text. Anything unrecognised
    // must become `null` — a mapping that silently kept an unknown string would grant nothing and
    // look configured.
    for (const r of ROLES) expect(asRole(r)).toBe(r);
    expect(asRole('auditor')).toBeNull();
    expect(asRole('')).toBeNull();
    expect(asRole(null)).toBeNull();
    expect(asRole(undefined)).toBeNull();
  });
});
