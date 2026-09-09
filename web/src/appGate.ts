// SPDX-License-Identifier: AGPL-3.0-only
// Which of four things the app renders before anything else: a spinner, a login form, the public
// board on its own, or the signed-in shell.
//
// A `.ts` with a test because the table has a row that used to be wrong in a way nobody could see.

import type { ConfigStatus } from './store';

/** What the top-level render should be. */
export type AppView =
  /** The config fetch has not settled. */
  | 'loading'
  /** Sign-in form: this deployment is private, or we could not find out. */
  | 'login'
  /** The public board with no shell around it — an anonymous visitor on a public deployment. */
  | 'public'
  /** The full application. */
  | 'app';

/**
 * Decide the view from the deployment's config and whether a token is held.
 *
 * 🚨 **Unknown config is closed, not public.** `App.tsx` used to `.catch()` the config fetch and
 * substitute `{ public_dashboard: true }`, so a core that was down, upgrading or misconfigured
 * dropped every visitor straight into the app shell with no sign-in screen and every panel
 * erroring. Nothing leaked — the API answers 401 either way — but the screen was wrong in the one
 * moment an operator most needs it to be honest about what is happening.
 *
 * ⚠️ A holder of a token still gets the app when the config is unreachable. They signed in already,
 * every screen shows its own error, and locking them out would remove the pages that say *why* the
 * deployment is unwell.
 */
export function appView(status: ConfigStatus, publicDashboard: boolean, authed: boolean): AppView {
  if (status === 'loading') return 'loading';
  if (authed) return 'app';
  if (status === 'ready' && publicDashboard) return 'public';
  return 'login';
}
