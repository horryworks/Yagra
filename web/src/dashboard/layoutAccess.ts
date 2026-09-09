// SPDX-License-Identifier: AGPL-3.0-only
// Which of the three boards this viewer may read, and whether they may save.
//
// It lives here rather than in `layoutStore.ts` for the reason every judgement in this repo does:
// Vitest runs `src/**/*.test.ts` in a node environment and never loads a `.tsx`, and `layoutStore`
// pulls in `registry.tsx` — 48 React components — so a test importing it would drag the whole
// widget catalog into a node process. A three-line decision with a real failure mode belongs
// somewhere a test can reach it.
//
// The failure it exists for is already shipped: `load()` skipped the fetch whenever there was no
// token, which is right for My Dashboard (keyed by account, 401 without one) and **wrong for the
// shared board**, whose GET is open on a public deployment. So an anonymous visitor was shown the
// hardcoded default five widgets while the admin's composed board sat in the database, and nothing
// anywhere reported an error.

/** Which credential a board's read needs, mirroring the guard its API route takes. */
export type BoardGate =
  /** `Caller` — a real account. My Dashboard is keyed by username, so it stays closed even on a
   *  public deployment: an anonymous visitor has no row and would share one nameless layout with
   *  every other visitor. */
  | 'session'
  /** `RequireView` — any signed-in role, and open anonymously on a public deployment. The shared
   *  board. */
  | 'view'
  /** Always readable when the deployment is public: the public board's own layout, which is the
   *  bootstrap an anonymous page cannot draw itself without (`public_access::ALWAYS_OPEN`). */
  | 'public';

/** What the viewer is, from the two stores that know. */
export interface Viewer {
  /** A bearer token is present. Not "the token is valid" — an expired one still takes the
   *  authenticated path, and the server answers 401, which is the correct outcome. */
  hasSession: boolean;
  /** This deployment serves anonymous visitors (`GET /api/v1/config`'s `public_dashboard`). */
  publicDashboard: boolean;
}

/** May this viewer read the board behind `gate`? */
export function mayLoad(gate: BoardGate, v: Viewer): boolean {
  switch (gate) {
    case 'session':
      return v.hasSession;
    case 'view':
      return v.hasSession || v.publicDashboard;
    case 'public':
      return v.hasSession || v.publicDashboard;
  }
}

/** May this viewer save a board?
 *
 * 🚨 Session only — **never a permission**. `useCan` is fail-closed while the role matrix is still
 * loading, so gating the save on `manage_config` would silently discard an admin's edit in the
 * window before that request lands. Which boards a signed-in user may write is enforced where it
 * belongs: by the API's guard, and by the UI not drawing a Customize button they cannot use.
 */
export function maySave(v: Viewer): boolean {
  return v.hasSession;
}
