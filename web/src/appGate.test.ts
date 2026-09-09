// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { LOGIN_PATH, appView } from './appGate';

/** Any path that is not the login form. */
const ELSEWHERE = '/dashboard';

describe('appView', () => {
  it('waits while the config is still loading', () => {
    expect(appView('loading', false, false, ELSEWHERE)).toBe('loading');
    expect(appView('loading', true, true, ELSEWHERE)).toBe('loading');
  });

  it('gives an anonymous visitor the public board on a public deployment', () => {
    expect(appView('ready', true, false, ELSEWHERE)).toBe('public');
    expect(appView('ready', true, false, '/settings/users')).toBe('public');
  });

  it('gives an anonymous visitor the login form on a private deployment', () => {
    expect(appView('ready', false, false, ELSEWHERE)).toBe('login');
  });

  it('always serves the login form at /login, even on a public deployment', () => {
    // 🚨 The defect Tier2a caught on the first real public deployment. Without this, a public
    // deployment answered every URL with the public board — /login included — so there was no way
    // for an operator to sign in at all, and the only recovery was to make it private again from a
    // browser that already held a session.
    expect(appView('ready', true, false, LOGIN_PATH)).toBe('login');
    expect(appView('unreachable', true, false, LOGIN_PATH)).toBe('login');
  });

  it('still waits at /login while the config is loading', () => {
    // Not a special case worth breaking: the form needs `auth_available` and `sso_enabled` to know
    // which buttons to draw, so rendering it early would flash the wrong set.
    expect(appView('loading', true, false, LOGIN_PATH)).toBe('loading');
  });

  it('treats an unreachable config as private, never as public', () => {
    // 🚨 The shipped bug. The old code substituted `{ public_dashboard: true }` on a failed fetch,
    // so a core that was down put every visitor into the app shell with no login screen and every
    // panel erroring. Unknown is closed.
    expect(appView('unreachable', false, false, ELSEWHERE)).toBe('login');
    expect(appView('unreachable', true, false, ELSEWHERE)).toBe('login');
  });

  it('keeps a token holder in the app even when the config is unreachable', () => {
    // They are already signed in, and the screens that would explain the outage live inside the
    // shell. Locking them out would remove the diagnosis along with the symptom.
    expect(appView('unreachable', false, true, ELSEWHERE)).toBe('app');
  });

  it('never shows the bare public board to someone holding a token', () => {
    // An admin composing the board reaches it through the normal route, with the header and the
    // banner. The chromeless shell is for visitors who have no other page at all.
    expect(appView('ready', true, true, ELSEWHERE)).toBe('app');
  });
});
