// SPDX-License-Identifier: AGPL-3.0-only
// The account kinds on Settings ▸ Users: which an admin may create, and which carry an
// explanation on their badge (ADR-200).
//
// A `.ts` so the coverage test reaches both lists without loading the page.

import type { UserKind } from '../types/api';

/** The account kinds an admin can create here.
 *
 *  A deliberate subset of `USER_KINDS`, the way `monitorKinds.ts` is a subset of `NodeKind`: an
 *  `oidc` or `ldap` account is provisioned by someone signing in, and the API refuses to create one
 *  directly. Offering it would be a choice that always fails. */
export const CREATABLE_USER_KINDS = ['local', 'service'] as const satisfies readonly UserKind[];

export type CreatableUserKind = (typeof CREATABLE_USER_KINDS)[number];

/** What pressing an account's kind badge explains — who holds its password and who decides its
 *  role, the question an `SSO` or `Directory` badge raises. `null` for `local`, which draws no
 *  badge. A `Record` so a new kind is a compile error until it is answered; the key strings are
 *  written here once (ADR-200 G8). */
export const USER_KIND_INFO: Record<UserKind, string | null> = {
  local: null,
  oidc: 'access:users.kindInfo.oidc.info',
  ldap: 'access:users.kindInfo.ldap.info',
  service: 'access:users.kindInfo.service.info',
};

/** The explanation behind an account's kind badge, or `null` when there is none: a plain local
 *  account, or a kind a newer server sends that this build does not know (it still gets its
 *  badge, with nothing to press). `auth_source` arrives as a plain string, hence the guard. */
export function kindInfoKey(kind: string): string | null {
  return Object.hasOwn(USER_KIND_INFO, kind) ? USER_KIND_INFO[kind as UserKind] : null;
}
