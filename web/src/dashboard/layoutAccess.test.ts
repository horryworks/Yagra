// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { mayLoad, maySave, type Viewer } from './layoutAccess';

const anonPublic: Viewer = { hasSession: false, publicDashboard: true };
const anonPrivate: Viewer = { hasSession: false, publicDashboard: false };
const signedIn: Viewer = { hasSession: true, publicDashboard: false };

describe('mayLoad', () => {
  it('keeps My Dashboard closed to an anonymous visitor even on a public deployment', () => {
    // The API agrees (`Caller` is not opened by the public switch), so fetching would 401. The
    // point of asking here is not to avoid the error — it is that there is no row to fetch: a
    // per-account board has no meaning without an account.
    expect(mayLoad('session', anonPublic)).toBe(false);
    expect(mayLoad('session', anonPrivate)).toBe(false);
    expect(mayLoad('session', signedIn)).toBe(true);
  });

  it('loads the shared board for an anonymous visitor on a public deployment', () => {
    // 🚨 The shipped bug this file exists for. `GET /api/v1/shared-dashboard` is `RequireView`, so
    // it is open on a public deployment — but the store checked for a token and rendered the
    // hardcoded default instead, silently.
    expect(mayLoad('view', anonPublic)).toBe(true);
    expect(mayLoad('view', anonPrivate)).toBe(false);
    expect(mayLoad('view', signedIn)).toBe(true);
  });

  it('loads the public board whenever there is anything to show it to', () => {
    expect(mayLoad('public', anonPublic)).toBe(true);
    expect(mayLoad('public', signedIn)).toBe(true);
    // Private and anonymous: there is no page at all, so nothing asks — and the API would 401.
    expect(mayLoad('public', anonPrivate)).toBe(false);
  });
});

describe('maySave', () => {
  it('is decided by the session alone, never by a permission', () => {
    // 🚨 If this ever consults the role matrix, an admin's edit is discarded during the window
    // before `GET /api/v1/roles` lands — `useCan` answers false while it is still resolving.
    expect(maySave(signedIn)).toBe(true);
    expect(maySave(anonPublic)).toBe(false);
    expect(maySave(anonPrivate)).toBe(false);
  });

  it('does not take a permission argument at all', () => {
    // Stated as a test rather than a comment: the signature is the guarantee. A future caller
    // cannot pass a permission in without changing this line, which is the moment to re-read why.
    expect(maySave.length).toBe(1);
  });
});
