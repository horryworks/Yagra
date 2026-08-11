// SPDX-License-Identifier: AGPL-3.0-only
/** The LDAP/AD directory settings form's pure half (ADR-041).
 *
 *  Mirrors the Rust side deliberately and narrowly: `ldap::validate` is the authority and answers
 *  with a typed 400, so what lives here is only what makes the form usable before a round trip. Two
 *  rules are load-bearing enough to be worth stating twice, and both are tested:
 *
 *  * **The bind password is two-valued, not three.** `aiConfigForm.ts` treats `''` as "clear the
 *    stored credential"; here an empty bind password is an *unauthenticated* bind, which a
 *    permissive directory answers `success` — so it is an error, never a value.
 *  * **`{username}` in the user filter.** Without it the filter matches the whole subtree.
 *
 *  What is deliberately **not** mirrored: whether a given security mode or attribute is workable.
 *  The server's Test button answers that against the real directory, and a second opinion here
 *  would be a mirror with nothing keeping it honest. */

import type { LdapConfigInput, LdapConfigView, LdapSecurity, Role } from '../types/api';
import { fromRoleMapRows, type RoleMapRow } from './roleMapForm';

export const DEFAULT_LDAPS_PORT = 636;
export const DEFAULT_STARTTLS_PORT = 389;

/** The editable state of the directory card. */
export interface LdapFormState {
  host: string;
  port: string;
  security: LdapSecurity;
  caCert: string;
  bindDn: string;
  bindPassword: string;
  replacePassword: boolean;
  userBaseDn: string;
  userFilter: string;
  usernameAttribute: string;
  uidAttribute: string;
  memberOfAttribute: string;
  groupBaseDn: string;
  groupFilter: string;
  groupNameAttribute: string;
  defaultRole: Role | '';
  enabled: boolean;
}

/** The starting state for a directory that has never been configured. AD's spellings, because the
 *  "AD" half of LDAP/AD is what most closed networks actually run. */
export function emptyLdapForm(): LdapFormState {
  return {
    host: '',
    port: String(DEFAULT_LDAPS_PORT),
    security: 'ldaps',
    caCert: '',
    bindDn: '',
    bindPassword: '',
    replacePassword: false,
    userBaseDn: '',
    userFilter: '(&(objectClass=user)(sAMAccountName={username}))',
    usernameAttribute: 'sAMAccountName',
    uidAttribute: 'objectGUID',
    memberOfAttribute: 'memberOf',
    groupBaseDn: '',
    groupFilter: '',
    groupNameAttribute: 'cn',
    defaultRole: '',
    enabled: false,
  };
}

/** Load a saved configuration into the form. The bind password is never returned, so the field
 *  starts empty and hidden behind the replace checkbox. */
export function toLdapForm(view: LdapConfigView): LdapFormState {
  return {
    host: view.host,
    port: String(view.port),
    security: view.security,
    caCert: view.ca_cert ?? '',
    bindDn: view.bind_dn,
    bindPassword: '',
    replacePassword: false,
    userBaseDn: view.user_base_dn,
    userFilter: view.user_filter,
    usernameAttribute: view.username_attribute,
    uidAttribute: view.uid_attribute,
    memberOfAttribute: view.member_of_attribute,
    groupBaseDn: view.group_base_dn ?? '',
    groupFilter: view.group_filter ?? '',
    groupNameAttribute: view.group_name_attribute,
    defaultRole: (view.default_role as Role | null) ?? '',
    enabled: view.enabled,
  };
}

/** The conventional port for a security mode, used to follow the mode when the operator has not
 *  overridden it. */
export function defaultPortFor(security: LdapSecurity): number {
  return security === 'starttls' ? DEFAULT_STARTTLS_PORT : DEFAULT_LDAPS_PORT;
}

/** Whether the password field should be shown at all: always before the first save, and afterwards
 *  only when the operator ticks "replace". Same shape as `keyIsEditable` in `aiConfigForm.ts`. */
export function passwordIsEditable(stored: LdapConfigView | null, form: LdapFormState): boolean {
  return !stored?.has_bind_password || form.replacePassword;
}

/** The URL the server will dial, for the form to show. Derived exactly as `LdapConfig::url` does —
 *  including the brackets a v6 literal needs — so the operator can see that choosing StartTLS is
 *  what makes it `ldap://`, and that there is no way to ask for a plaintext `ldaps` host. */
export function connectionUrl(form: LdapFormState): string {
  const scheme = form.security === 'starttls' ? 'ldap' : 'ldaps';
  const host = form.host.trim();
  const authority = host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
  return `${scheme}://${authority}:${form.port.trim()}`;
}

/** Every reason a form cannot be saved. `as const` so the i18n coverage test can walk it: the page
 *  renders `t(`ldap.err.${problem}`)` with no fallback, and a code added without strings would be
 *  the only thing an admin is told about why sign-in configuration will not save. */
export const LDAP_FORM_PROBLEMS = [
  'host',
  'port',
  'bindDn',
  'bindPassword',
  'userBaseDn',
  'userFilter',
  'groupPair',
  'groupFilter',
  'noMapping',
] as const;

/** Why a form cannot be saved. A code rather than a sentence, so both locales own the wording. */
export type LdapFormProblem = (typeof LDAP_FORM_PROBLEMS)[number];

/** Validate the form, returning the first problem or `null`. */
export function validateLdapForm(
  form: LdapFormState,
  rows: readonly RoleMapRow[],
  stored: LdapConfigView | null,
): LdapFormProblem | null {
  if (!form.host.trim()) return 'host';
  const port = Number(form.port.trim());
  if (!Number.isInteger(port) || port < 1 || port > 65535) return 'port';
  if (!form.bindDn.trim()) return 'bindDn';
  // Required on a first save, and required again whenever "replace" is ticked. Never optional-blank:
  // a blank bind password is an anonymous search, not an absent credential.
  if (passwordIsEditable(stored, form) && !form.bindPassword.trim()) return 'bindPassword';
  if (!form.userBaseDn.trim()) return 'userBaseDn';
  if (!form.userFilter.includes('{username}')) return 'userFilter';

  const hasBase = form.groupBaseDn.trim().length > 0;
  const hasFilter = form.groupFilter.trim().length > 0;
  if (hasBase !== hasFilter) return 'groupPair';
  if (
    hasFilter &&
    !form.groupFilter.includes('{user_dn}') &&
    !form.groupFilter.includes('{username}')
  ) {
    return 'groupFilter';
  }
  // Enabling with nothing to map and no default denies every login while the page looks correctly
  // filled in — the server refuses it too, but catching it here says so before a round trip.
  if (form.enabled && Object.keys(fromRoleMapRows(rows)).length === 0 && !form.defaultRole) {
    return 'noMapping';
  }
  return null;
}

/** Build the save payload.
 *
 *  `bind_password` is **omitted entirely** when it is not being replaced — that omission is what
 *  makes the server's "keep the stored one" branch fire, so sending `''` instead would be rejected
 *  rather than ignored. */
export function toLdapInput(
  form: LdapFormState,
  rows: readonly RoleMapRow[],
  stored: LdapConfigView | null,
): LdapConfigInput {
  const optional = (s: string): string | null => (s.trim() ? s.trim() : null);
  return {
    host: form.host.trim(),
    port: Number(form.port.trim()),
    security: form.security,
    ca_cert: optional(form.caCert),
    bind_dn: form.bindDn.trim(),
    ...(passwordIsEditable(stored, form) ? { bind_password: form.bindPassword } : {}),
    user_base_dn: form.userBaseDn.trim(),
    user_filter: form.userFilter.trim(),
    username_attribute: form.usernameAttribute.trim(),
    uid_attribute: form.uidAttribute.trim(),
    member_of_attribute: form.memberOfAttribute.trim(),
    group_base_dn: optional(form.groupBaseDn),
    group_filter: optional(form.groupFilter),
    group_name_attribute: form.groupNameAttribute.trim(),
    role_map: fromRoleMapRows(rows),
    default_role: form.defaultRole || null,
    enabled: form.enabled,
  };
}
