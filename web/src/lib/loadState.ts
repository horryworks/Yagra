// SPDX-License-Identifier: AGPL-3.0-only
// Why a screen has no rows — for the cases where the answer is not "there are none" (ADR-056).
//
// 🚨 **A Viewer was shown "No credentials yet" while the deployment held two.** `CredentialsPage`'s
// `.catch` handled `admin_unavailable` and let the `403` fall through, so `rows` stayed `[]`,
// `loading` went false, and `DataTable` drew its empty state. **A failure that arrives in the shape
// of a success** — no test failed, nothing was logged, and the screen looked healthy. It shipped
// that way from the first design-system commit and was only found by signing in as a Viewer.
//
// Fourteen pages held a byte-identical copy of that `.catch` (`extensibility.md` §3), so the
// judgement lives here, once, and `loadState.test.ts` fails any page that hand-rolls it again.
//
// **This file is `.ts`, not `.tsx`, and that is load-bearing.** Vitest runs with
// `environment: 'node'` and `include: ['src/**/*.test.ts']`, so a decision written inside a
// component is a decision no test can reach (`testing.md`).
import { ApiError } from '../services/api';

/** The reasons a list can be empty that are **not** "there is nothing to list". */
export const LOAD_BLOCKS = ['unavailable', 'forbidden'] as const;
export type LoadBlock = (typeof LOAD_BLOCKS)[number];

/**
 * Classify a failed load into the reason the screen should state, or `null` to leave the screen
 * alone.
 *
 * - `unavailable` — `503 admin_unavailable`: the deployment has no admin state (skeleton mode).
 *   Nothing is wrong with the caller; the feature is not present.
 * - `forbidden` — `403`: the caller is authenticated and lacks the permission.
 * - `null` — anything else, **including `401`**. A `401` is handled globally (the app drops auth
 *   state and routes to sign-in), so a page that also drew a block would flash a wrong explanation
 *   on the way out. Two pages used to spell this as `else if (status === 401) setUnavailable(false)`;
 *   returning `null` says the same thing once.
 *
 * ⚠️ **The server is the only source of truth for `forbidden`** (ADR-056 decision 3). Do not add a
 * branch that infers it from the signed-in role: that is a second copy of the permission matrix,
 * and the copy fails *open* — it would show a table the API then refuses to fill.
 *
 * Both the status and the code are checked, because `ApiError::forbidden_code` exists for refusals
 * the client should tell apart from a plain role failure; those carry a different `code` on the
 * same status, and they are still a refusal.
 */
export function classifyLoadError(e: unknown): LoadBlock | null {
  if (!(e instanceof ApiError)) return null;
  if (e.code === 'admin_unavailable') return 'unavailable';
  if (e.status === 403 || e.code === 'forbidden') return 'forbidden';
  return null;
}
