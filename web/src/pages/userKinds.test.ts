// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { USER_KINDS } from '../types/api';
import { CREATABLE_USER_KINDS, USER_KIND_INFO } from './userKinds';

describe('account kinds', () => {
  it('offers only kinds the API knows, and never one provisioned by signing in', () => {
    for (const k of CREATABLE_USER_KINDS) expect(USER_KINDS).toContain(k);
    expect(CREATABLE_USER_KINDS).not.toContain('oidc');
    expect(CREATABLE_USER_KINDS).not.toContain('ldap');
  });

  it('explains every kind that draws a badge, and only those', () => {
    // The list draws a badge for every kind but `local`, so that is the one with nothing to press.
    for (const k of USER_KINDS) {
      if (k === 'local') expect(USER_KIND_INFO[k]).toBeNull();
      else expect(USER_KIND_INFO[k]).toBe(`access:users.kindInfo.${k}.info`);
    }
  });
});
