// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { appView } from './appGate';

describe('appView', () => {
  it('waits while the config is still loading', () => {
    expect(appView('loading', false, false)).toBe('loading');
    expect(appView('loading', true, true)).toBe('loading');
  });

  it('gives an anonymous visitor the public board on a public deployment', () => {
    expect(appView('ready', true, false)).toBe('public');
  });

  it('gives an anonymous visitor the login form on a private deployment', () => {
    expect(appView('ready', false, false)).toBe('login');
  });

  it('treats an unreachable config as private, never as public', () => {
    // 🚨 The shipped bug. The old code substituted `{ public_dashboard: true }` on a failed fetch,
    // so a core that was down put every visitor into the app shell with no login screen and every
    // panel erroring. Unknown is closed.
    expect(appView('unreachable', false, false)).toBe('login');
    expect(appView('unreachable', true, false)).toBe('login');
  });

  it('keeps a token holder in the app even when the config is unreachable', () => {
    // They are already signed in, and the screens that would explain the outage live inside the
    // shell. Locking them out would remove the diagnosis along with the symptom.
    expect(appView('unreachable', false, true)).toBe('app');
  });

  it('never shows the bare public board to someone holding a token', () => {
    // An admin composing the board reaches it through the normal route, with the header and the
    // banner. The chromeless shell is for visitors who have no other page at all.
    expect(appView('ready', true, true)).toBe('app');
  });
});
