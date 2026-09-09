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
  /** The public board with no shell around it — an anonymous visitor who asked for it. */
  | 'public'
  /** The full application. */
  | 'app';

/** The sign-in form, and where an anonymous visitor lands whatever they asked for. */
export const LOGIN_PATH = '/login';

/** The one path that serves the public board to someone with no session.
 *
 *  It is the same path the signed-in editor uses (`routes.tsx`), and that is deliberate: one
 *  address, two audiences. An admin gets the board inside the shell with the banner and the
 *  controls; a visitor gets the board and nothing else. */
export const PUBLIC_PATH = '/dashboard/public';

/**
 * Decide the view from the deployment's config, the path asked for, and whether a token is held.
 *
 * **An anonymous visitor lands on the sign-in form, always.** The public board is somewhere they
 * are *sent*, from the button on that form, rather than somewhere they arrive. The two orderings
 * are not equivalent and the difference is who the deployment appears to be for: a monitoring
 * system that answers the front door with a board reads as a public site, and the operator who
 * needs to sign in has to be told a URL. This way both audiences get an obvious next click.
 *
 * 🚨 **The first version had no `path` argument at all**, so on a public deployment an anonymous
 * visitor got the public board *for every URL* — including `/login`. There was then **no way for
 * an operator to sign in**: the form had no route that could reach it, and the only recovery was
 * to turn the deployment private again from a machine that was already signed in. Tier2a caught it
 * on the first public deployment, which is the whole reason that suite drives a real box. The
 * table below cannot fall into that again — `login` is what every unrecognized path returns.
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
  // The board is served at one address and only when the deployment says it is published. Every
  // other path an anonymous visitor asks for — `/`, a deep link, a typo — is the sign-in form.
  if (status === 'ready' && publicDashboard && path === PUBLIC_PATH) return 'public';
  return 'login';
}
