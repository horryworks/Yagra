// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { LOGIN_PATH, PUBLIC_PATH, appView } from './appGate';

/** Any path that is neither the login form nor the public board. */
const ELSEWHERE = '/dashboard';

describe('appView', () => {
  it('waits while the config is still loading', () => {
    expect(appView('loading', false, false, ELSEWHERE)).toBe('loading');
    expect(appView('loading', true, true, ELSEWHERE)).toBe('loading');
    expect(appView('loading', true, false, PUBLIC_PATH)).toBe('loading');
  });

  it('lands an anonymous visitor on the sign-in form, even on a public deployment', () => {
    // The board is somewhere a visitor is sent from that form, not where they arrive (Inc.1).
    expect(appView('ready', true, false, '/')).toBe('login');
    expect(appView('ready', true, false, ELSEWHERE)).toBe('login');
    expect(appView('ready', true, false, '/settings/users')).toBe('login');
  });

  it('serves the public board at its own path on a public deployment', () => {
    expect(appView('ready', true, false, PUBLIC_PATH)).toBe('public');
  });

  it('does not serve the board at that path on a private deployment', () => {
    // The same address the editor uses, so it is linkable and typo-able. Publishing is the only
    // thing that opens it — the API agrees, and answers 401 for the layout either way.
    expect(appView('ready', false, false, PUBLIC_PATH)).toBe('login');
    expect(appView('unreachable', true, false, PUBLIC_PATH)).toBe('login');
  });

  it('always serves the login form at /login', () => {
    // 🚨 The defect Tier2a caught on the first real public deployment. The version before it had no
    // `path` argument at all, so a public deployment answered every URL with the board — /login
    // included — and there was no way for an operator to sign in. Now unreachable-by-default runs
    // the other way: everything that is not the board's own path is the form.
    expect(appView('ready', true, false, LOGIN_PATH)).toBe('login');
    expect(appView('unreachable', true, false, LOGIN_PATH)).toBe('login');
  });

  it('still waits at /login while the config is loading', () => {
    // Not a special case worth breaking: the form needs `auth_available` and `sso_enabled` to know
    // which buttons to draw — and, since Inc.1, whether to offer the public board at all.
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
    // One address, two audiences: an admin at the board's path gets it inside the shell, with the
    // header, the banner and the editing controls. The chromeless shell is for visitors only.
    expect(appView('ready', true, true, ELSEWHERE)).toBe('app');
    expect(appView('ready', true, true, PUBLIC_PATH)).toBe('app');
  });
});
