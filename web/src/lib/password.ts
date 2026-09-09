// SPDX-License-Identifier: AGPL-3.0-only
// Everything the two password forms decide, in one place a test can run (ADR-122 決定 8).
//
// It lives in a `.ts` rather than beside either dialog because Vitest runs `src/**/*.test.ts` in
// the node environment and never loads a `.tsx` — judgement written in a component is judgement
// nothing executes (`.claude/rules/testing.md`), and `tsxJudgement.test.ts` fails the build for it.
//
// ⚠️ `MIN_PW` is still a copy of the backend's `MIN_PASSWORD_LEN` (`api/users.rs`), and nothing
// pins the two together. What this file removes is the *third* copy: `UsersPage.tsx` had its own
// literal `8`, and the account dialog would have made a fourth. Raising the floor is now two edits
// (here and in Rust) rather than three — which is the honest description, not "one place".

import type { UserKind } from '../types/api';

/** Minimum password length the API accepts. Mirrors `MIN_PASSWORD_LEN` in `api/users.rs`. */
export const MIN_PW = 8;

/** Why a change-password form cannot be submitted yet, or `null` when it can. */
export type PasswordFormProblem = 'current-missing' | 'too-short' | 'unchanged' | 'mismatch' | null;

/**
 * Validate a self-service change (current + new + confirmation). Returns the reason it cannot be
 * submitted, in the order the operator fills the form in — so the message that appears is about the
 * field they are looking at rather than the first one that happens to be wrong.
 *
 * This is about *submitting*. Whether to draw the message is the caller's: a mismatch is not worth
 * saying out loud while the confirmation box is still empty.
 *
 * `unchanged` is checked here as well as on the server. The server has to refuse it (nothing says
 * the WebUI is the only client), and refusing it here is what keeps the operator from being signed
 * out for a change that changed nothing.
 */
export function validateOwnPasswordChange(form: {
  current: string;
  next: string;
  confirm: string;
}): PasswordFormProblem {
  if (form.current.length === 0) return 'current-missing';
  if (form.next.length < MIN_PW) return 'too-short';
  if (form.next === form.current) return 'unchanged';
  if (form.next !== form.confirm) return 'mismatch';
  return null;
}

/**
 * Whether Yagra holds this account's password at all.
 *
 * `null` is `/auth/me`'s skeleton-mode answer and reads as "no" — every consumer fails closed,
 * because the alternative is drawing a control whose write path does not exist here. `service` is a
 * no for a different reason (it cannot sign in), and both reasons end in the same place.
 */
export function hasLocalPassword(kind: UserKind | null): boolean {
  return kind === 'local';
}

/**
 * The i18n key explaining where a non-local account's password actually lives, or `null` when there
 * is nothing to explain.
 *
 * This exists so that removing the menu item does not leave the person who came looking for it with
 * no answer (ADR-055 R6) — hiding a control is only half of "say it where they look".
 *
 * Two explicit keys rather than one built from the enum (`` t(`shell.kind.${kind}`) ``): a runtime
 * key would owe a row in `i18nEnumKeys.test.ts` and strings for the two kinds that have nothing to
 * say. The `never` arm is what makes a fifth `UserKind` a compile error rather than a silent `null`.
 */
export function passwordHomeKey(
  kind: UserKind | null,
): 'shell.passwordAtIdp' | 'shell.passwordAtDirectory' | null {
  if (kind === null) return null;
  switch (kind) {
    case 'oidc':
      return 'shell.passwordAtIdp';
    case 'ldap':
      return 'shell.passwordAtDirectory';
    // A local account has the control instead, and nobody reads this from a service account —
    // it cannot sign in, so it never holds the session this is drawn from.
    case 'local':
    case 'service':
      return null;
    default: {
      const unreachable: never = kind;
      return unreachable;
    }
  }
}
