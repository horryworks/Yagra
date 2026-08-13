// SPDX-License-Identifier: AGPL-3.0-only
// Audit-row display parsing.

import { describe, expect, it } from 'vitest';
import { parseAction } from './auditRow';

describe('parseAction', () => {
  it('splits a method and path on the first space only', () => {
    // A query string can contain spaces; everything after the first one is the path.
    expect(parseAction('POST /api/v1/nodes')).toEqual({
      method: 'POST',
      path: '/api/v1/nodes',
      login: false,
    });
    expect(parseAction('GET /api/v1/events?q=link down').path).toBe('/api/v1/events?q=link down');
  });

  it('labels the sign-in entry rather than showing its internal token', () => {
    expect(parseAction('auth.login')).toEqual({ method: 'SIGN IN', path: null, login: true });
  });

  it('labels every sign-in method, not just the local one', () => {
    // These three were rendered as raw `auth.login.ldap` chips and missed by the "Sign in" filter,
    // because this matched `=== 'auth.login'` while `api/session.rs` had documented the family as a
    // prefix. The backend's AuditAction::Login filters on the same prefix, so the chip an operator
    // sees and the rows the filter returns now cannot disagree.
    for (const action of [
      'auth.login.ldap',
      'auth.login.ldap_unavailable',
      'auth.login.ldap_conflict',
    ]) {
      expect(parseAction(action), action).toEqual({ method: 'SIGN IN', path: null, login: true });
    }
  });

  it('does not swallow an unrelated action that merely starts with "auth."', () => {
    expect(parseAction('auth.logout').login).toBe(false);
  });

  it('shows a bare action name as-is instead of inventing a path', () => {
    expect(parseAction('auth.logout')).toEqual({
      method: 'auth.logout',
      path: null,
      login: false,
    });
    expect(parseAction('')).toEqual({ method: '', path: null, login: false });
  });
});

// The CSV half of this file is gone with the client-side export. Settings ▸ Audit now downloads
// `GET /api/v1/audit/export.csv`, so the encoder that matters for THIS page is the Rust one —
// `crates/yagra-core/src/api/audit.rs::the_export_neutralizes_a_username_a_stranger_chose` carries
// the reachable path these cases described: a failed sign-in stores the submitted username
// verbatim, so the value is chosen by an unauthenticated caller and read back by an admin opening
// the file. `lib/csv.ts` and its tests remain for the Troubleshoot report export.
