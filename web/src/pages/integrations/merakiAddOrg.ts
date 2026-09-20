// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind the Meraki "Add organization" dialog (ADR-164 Inc.6).
//
// Since Inc.6 an organization can be added under a key that is already stored, so one key serves
// every organization it can see instead of being sealed again as a second credential each time.
// That gives the dialog decisions it did not have before: which of two key sources the request
// names, which region a stored key belongs to, and which of the discovered organizations may still
// be added. They are here rather than in `MerakiIntegrationPage.tsx` because Vitest never loads a
// `.tsx` (`testing.md`) — and the first of them is a contract with the server, not layout.

import { sortRows } from '../../lib/tableSort';
import type { CredentialSummary, MerakiOrg, MerakiOrgOption } from '../../types/api';
import { MERAKI_REGIONS } from './merakiRegions';

/** Where the dialog's API key comes from: typed into the box, or picked from the stored ones. */
export type KeySourceKind = 'typed' | 'saved';

/** The stored Meraki keys, as the picker's options.
 *
 *  Only `meraki_api`: every other kind is a secret for something else, and offering one would send
 *  an SNMP community's id where the server expects a Dashboard key (it answers
 *  `400 invalid_credential`). Sorted by name through the table sort rather than a comparator of
 *  its own, so the keys come in the order the Credentials table starts in — without regard to
 *  case, and `key-2` before `key-10`. */
export function savedMerakiKeys(creds: readonly CredentialSummary[]): CredentialSummary[] {
  return sortRows(
    creds.filter((c) => c.kind === 'meraki_api'),
    { by: 'name', dir: 'asc' },
    { name: (c) => c.name },
  );
}

/** The ONE key field a discover or create request carries, or `null` while the chosen source is
 *  not usable yet (a blank box, or no stored key picked).
 *
 *  🚨 Never both. The server refuses a body naming both (`400 invalid_request`) and one naming
 *  neither (`400 invalid_api_key`), so both requests spread this value into their body instead of
 *  reading the two pieces of state themselves — a key typed before switching to "Use a saved key"
 *  is still in the box's state, and must not ride along. The typed key is trimmed the way the
 *  server trims it (`api/meraki.rs::key_source`), so "blank" means the same thing on both sides
 *  and the button is never enabled for a request that can only be refused. */
export function keyFields(
  source: KeySourceKind,
  apiKey: string,
  credentialId: string,
): { api_key: string } | { credential_id: string } | null {
  if (source === 'saved') return credentialId ? { credential_id: credentialId } : null;
  const key = apiKey.trim();
  return key ? { api_key: key } : null;
}

/** The region a stored key belongs to: the one an organization already using it is polled in.
 *
 *  A Dashboard API key is issued by one Meraki cloud (global, Canada, China, US Gov) and is
 *  unknown to the others, so the region follows the key rather than being a second thing the
 *  operator has to remember. `fallback` answers for a key no organization uses yet. The dialog
 *  only *sets* its region select from this; the operator can still change it. */
export function regionForSavedKey(
  credentialId: string,
  orgs: readonly Pick<MerakiOrg, 'credential_id' | 'base_url'>[],
  fallback: string,
): string {
  return orgs.find((o) => o.credential_id === credentialId)?.base_url ?? fallback;
}

/** A base URL the region select has no option for, or `null` when it is one of the offered ones.
 *
 *  An organization added through the API can carry any allow-listed URL (a trailing slash is
 *  enough to differ), and following a stored key can land the dialog on it. A `<select>` whose
 *  value matches no option *displays its first option* while the state keeps the other value — so
 *  the dialog would show "Global" and send something else. The caller renders this as one extra
 *  option, so what is shown is what is sent. */
export function unlistedRegion(baseUrl: string): string | null {
  return MERAKI_REGIONS.some((r) => r.base_url === baseUrl) ? null : baseUrl;
}

/** The organizations that can still be added: the default selection, and the only rows whose
 *  checkbox is enabled.
 *
 *  `POST /meraki/orgs` skips one that is monitored already rather than failing, so sending it
 *  would do no harm — but a ticked row the server then ignores is a dialog that says "Add 3" and
 *  adds 2. */
export function selectableOrgIds(options: readonly MerakiOrgOption[]): string[] {
  return options.filter((o) => !o.already_added).map((o) => o.id);
}

/** Whether the key found organizations and every one of them is monitored already — the dialog
 *  then says so instead of leaving a list of dead checkboxes over a disabled button.
 *
 *  ⚠️ Not simply "nothing is selectable": a key that can see *no* organization has nothing
 *  selectable either, and that is a different sentence (`meraki.addOrg.noOrgs`). */
export function allAlreadyAdded(options: readonly MerakiOrgOption[]): boolean {
  return options.length > 0 && options.every((o) => o.already_added);
}

/** The name of the stored credential holding an organization's key, or `null`.
 *
 *  `creds === null` is "this caller may not read credentials, or the list has not arrived" — the
 *  row then says nothing about the key, which is also the answer for a credential the list does
 *  not contain. Never the raw id: a uuid on the row would read as the key itself. */
export function keyNameFor(
  org: Pick<MerakiOrg, 'credential_id'>,
  creds: readonly Pick<CredentialSummary, 'id' | 'name'>[] | null,
): string | null {
  return creds?.find((c) => c.id === org.credential_id)?.name ?? null;
}
