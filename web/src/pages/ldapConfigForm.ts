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
import { fromRoleMapRows, toRoleMapRows, type RoleMapRow } from './roleMapForm';

export const DEFAULT_LDAPS_PORT = 636;
export const DEFAULT_STARTTLS_PORT = 389;

/** The editable state of the directory card. */
export interface LdapFormState {
  host: string;
  port: string;
  security: LdapSecurity;
  caCert: string;
  bindDn: string;
  /** A new bind password, or `''` for "keep the stored one" (`SecretInput`, ADR-200). */
  bindPassword: string;
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
 *  starts empty, which means "keep the stored one". */
export function toLdapForm(view: LdapConfigView): LdapFormState {
  return {
    host: view.host,
    port: String(view.port),
    security: view.security,
    caCert: view.ca_cert ?? '',
    bindDn: view.bind_dn,
    bindPassword: '',
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

/** The bind password a save sends: the typed one, or `undefined` to keep what is stored.
 *
 *  An empty (or blank) box is "keep", exactly as `SecretInput` shows it. It is never sent as `''`:
 *  a blank bind password is an anonymous search, and on a first save `validateLdapForm` refuses it
 *  before this is asked. */
export function bindPasswordToSend(form: LdapFormState): string | undefined {
  return form.bindPassword.trim() === '' ? undefined : form.bindPassword;
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
  // Required on a first save. Never optional-blank: a blank bind password is an anonymous search,
  // not an absent credential. Once one is stored, an empty box keeps it.
  if (!stored?.has_bind_password && !form.bindPassword.trim()) return 'bindPassword';
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
 *  `bind_password` is **omitted entirely** when none was typed — that omission is what makes the
 *  server's "keep the stored one" branch fire, so sending `''` instead would be rejected rather
 *  than ignored. */
export function toLdapInput(form: LdapFormState, rows: readonly RoleMapRow[]): LdapConfigInput {
  const optional = (s: string): string | null => (s.trim() ? s.trim() : null);
  const password = bindPasswordToSend(form);
  return {
    host: form.host.trim(),
    port: Number(form.port.trim()),
    security: form.security,
    ca_cert: optional(form.caCert),
    bind_dn: form.bindDn.trim(),
    ...(password !== undefined ? { bind_password: password } : {}),
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

/** Whether the form holds anything a save would change.
 *
 *  Asked of the payload rather than of each field, so it cannot disagree with what Save sends: a
 *  blank mapping row, trailing space and an untouched password box are not changes, because the
 *  save would not send them either. `true` when nothing is stored yet. */
export function ldapFormChanged(
  stored: LdapConfigView | null,
  form: LdapFormState,
  rows: readonly RoleMapRow[],
): boolean {
  if (stored == null) return true;
  const now = toLdapInput(form, rows);
  const saved = toLdapInput(toLdapForm(stored), toRoleMapRows(stored.role_map));
  return canonical(now) !== canonical(saved);
}

/** A payload as text with its role map in a fixed order, so two equal maps compare equal. */
function canonical(input: LdapConfigInput): string {
  const roleMap = Object.entries(input.role_map ?? {}).sort(([a], [b]) => a.localeCompare(b));
  return JSON.stringify({ ...input, role_map: roleMap });
}

/** Whether Test may be pressed. The check exercises the **stored** configuration, so it waits for a
 *  first save and for every later edit to be saved — a result would otherwise appear to describe
 *  what is on screen. This replaced the sentence "Save the directory before testing it" (ADR-200). */
export function canTestLdap(
  stored: LdapConfigView | null,
  form: LdapFormState,
  rows: readonly RoleMapRow[],
): boolean {
  return stored != null && !ldapFormChanged(stored, form, rows);
}

/** Whether saving this form switches off a directory that is on now — which revokes every directory
 *  account's sessions, a result nothing else on the screen shows. The warning is drawn only then
 *  (ADR-200 kind 3: a sentence shown when the dangerous choice is made). */
export function savingRevokesSessions(stored: LdapConfigView | null, form: LdapFormState): boolean {
  return stored?.enabled === true && !form.enabled;
}
