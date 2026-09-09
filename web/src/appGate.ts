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

/** The one path that is always the sign-in form, whatever else is true. */
export const LOGIN_PATH = '/login';

/**
 * Decide the view from the deployment's config, the path asked for, and whether a token is held.
 *
 * 🚨 **`/login` always wins, and leaving that out was a real defect.** The first version had no
 * `path` argument, so on a public deployment an anonymous visitor got the public board *for every
 * URL* — including `/login`. There was then **no way for an operator to sign in at all**: the form
 * had no route that could reach it, and the only recovery was to turn the deployment private again
 * from a machine that was already signed in. Tier2a caught it on the first public deployment,
 * which is the whole reason that suite drives a real box.
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
export function appView(
  status: ConfigStatus,
  publicDashboard: boolean,
  authed: boolean,
  path: string,
): AppView {
  if (status === 'loading') return 'loading';
  if (authed) return 'app';
  // Before the public board, and before the config has even been read: an operator who types
  // /login gets the form. A public deployment with no way in is a locked-out deployment.
  if (path === LOGIN_PATH) return 'login';
  if (status === 'ready' && publicDashboard) return 'public';
  return 'login';
}
