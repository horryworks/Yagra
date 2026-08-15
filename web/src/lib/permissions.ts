// SPDX-License-Identifier: AGPL-3.0-only
// "May the current principal do X?" — the one place the WebUI answers that (ADR-056 Inc.2).
//
// It holds no permission table of its own, and that is the whole point. `GET /api/v1/roles` returns
// the server's matrix, derived from `Permission::ALL × Role::ALL` in `yagra-common/src/rbac.rs`, so
// this file is a lookup: which role am I, and does the server say that role grants this permission.
// Writing `role === 'admin'` at a call site instead would create a second permission table in a
// language the compiler cannot compare to the first, and the two would drift the first time a
// permission moved between roles.
//
// Why it exists: every write control in the app used to be gated on `authed`, which answers "am I
// signed in", not "may I do this". A Viewer saw `+ Add window` and `+ Add mute`; an Operator saw
// every admin-only `+ Add` on the screens whose list is readable by anyone. The server refused all
// of them, so nothing happened that should not have — but offering an action and then refusing it
// is not a permission check, it is a dead end with a 403 at the bottom.
//
// Fail-closed by construction: an unknown role, a matrix that has not arrived yet, and a matrix
// that does not list the role all answer "no". That direction is the safe one — a control that
// appears a moment late costs nothing; one that appears and then 403s costs the operator a trip.
//
// **This file is `.ts`, not `.tsx`, and that is load-bearing**: Vitest runs `environment: 'node'`
// over `src/**/*.test.ts`, so judgement written inside a component is judgement no test can reach
// (`testing.md`).

import type { Permission, RoleMatrix } from '../types/api';

/**
 * Whether `role` grants `perm`, according to the server's own matrix.
 *
 * `null` for either input means "not resolved yet" and answers false, as does a role the matrix
 * does not list. A role this build has never heard of is either a newer server or a corrupt token;
 * both must grant nothing rather than be guessed at.
 */
export function grants(matrix: RoleMatrix | null, role: string | null, perm: Permission): boolean {
  if (matrix == null || role == null) return false;
  return matrix.roles.find((r) => r.key === role)?.permissions.includes(perm) ?? false;
}

/**
 * The human label for a permission, taken from the server's catalogue.
 *
 * Used where the UI has to name the privilege that is missing. The string is the server's — the one
 * `Roles & privileges` already renders verbatim — so the two surfaces cannot describe the same
 * permission differently. Falls back to the key, which is at least true.
 *
 * ⚠️ It is English on every locale, like the rest of that matrix. Translating it here would put the
 * catalogue in two places, and the second copy is the one that goes stale.
 */
export function permissionLabel(matrix: RoleMatrix | null, perm: Permission): string {
  return matrix?.permissions.find((p) => p.key === perm)?.label ?? perm;
}
