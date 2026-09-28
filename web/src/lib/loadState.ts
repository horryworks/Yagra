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
import { ApiError, errMsg } from '../services/api';

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

/** What a list screen knows about its read (ADR-184). `useLoad` holds one of these. */
export interface LoadState<T> {
  /** The last answer — kept through a later failure, so a re-read that fails leaves the list the
   *  operator was reading rather than blanking it. */
  data: T;
  /** Why the screen draws a notice instead of its list, or `null`. Decided again at every settle. */
  block: LoadBlock | null;
  /** A failure that is not a block, as text — only when the caller asked for one
   *  (`errorFallback`). Most lists deliberately show nothing for a 500; changing that is a
   *  separate decision, not this one's. */
  error: string | null;
  /** True until the first read settles, and never again: a re-read keeps the rows on screen. */
  loading: boolean;
}

/** The reducer's state: the screen's view plus the sequence number of the answer it shows. */
export interface LoadMachine<T> extends LoadState<T> {
  applied: number;
}

export type LoadEvent<T> =
  | { type: 'loaded'; seq: number; data: T }
  | { type: 'failed'; seq: number; error: unknown; fallback?: string }
  /** The read is switched off (`enabled: false`). Settles `loading` without a request. */
  | { type: 'skipped'; seq: number };

export function initialLoadState<T>(initial: T): LoadMachine<T> {
  return { data: initial, block: null, error: null, loading: true, applied: 0 };
}

/**
 * One read's answer, applied to the screen.
 *
 * **The request asked last wins** (`seq`): an answer older than the one on screen is dropped.
 * Screens used to apply whatever arrived, so a slow answer to the previous filter could land after
 * the answer to the current one and put the wrong rows back — a few had a `cancelled` flag against
 * that, most did not.
 */
export function loadReducer<T>(s: LoadMachine<T>, e: LoadEvent<T>): LoadMachine<T> {
  if (e.seq <= s.applied) return s;
  switch (e.type) {
    case 'loaded':
      return { data: e.data, block: null, error: null, loading: false, applied: e.seq };
    case 'failed': {
      const block = classifyLoadError(e.error);
      const error = block === null && e.fallback !== undefined ? errMsg(e.error, e.fallback) : null;
      return { ...s, block, error, loading: false, applied: e.seq };
    }
    case 'skipped':
      return { ...s, loading: false, applied: e.seq };
  }
}
