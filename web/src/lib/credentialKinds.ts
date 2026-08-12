// SPDX-License-Identifier: AGPL-3.0-only
// Which stored credential kinds each picker may offer.
//
// This was four hardcoded lists that had already drifted: Discovery's credential-finder offered
// `['snmp_v2c', 'snmp_v3']` while the two node-binding pickers offered `snmp_v2c` alone, so an
// SNMPv3 credential could be created, matched against a device during discovery, and then never
// bound to a node from either dialog — even though `scheduler.rs::resolve_snmp_auth` has always
// decrypted and used one. `ui-conventions.md` names these lists as ones to fold into a source
// rather than extend; this is that source.
//
// ⚠️ The SNMP list is an **allow-list, not a convenience filter**. `resolve_snmp_auth` reads a
// v3 credential as USM parameters and treats **any other kind's bytes as a community string** —
// so offering an `http_auth` or `meraki_api` credential here would put that secret's plaintext on
// the wire in an SNMP GET. The disjointness below is a security property, and it has a test.

/** Credential kinds an operator can create in Settings ▸ Credentials.
 *
 *  Not every kind that can exist: `meraki_api` rows are created by the Meraki integration and are
 *  shown read-only, so they are deliberately absent. */
export const CREDENTIAL_KINDS = ['snmp_v2c', 'snmp_v3', 'http_auth', 'api_token'] as const;

export type CredentialKind = (typeof CREDENTIAL_KINDS)[number];

/** Kinds that may be bound to a node as its SNMP credential, or tried by the credential finder.
 *  Both are consumed by `resolve_snmp_auth`, so they are one list. */
export const SNMP_CREDENTIAL_KINDS: readonly CredentialKind[] = ['snmp_v2c', 'snmp_v3'];

/** Kinds a URL monitor may present to the endpoint it probes. `http_auth` is the current kind;
 *  `api_token` predates it and is accepted as a bearer token (`secrets.rs::KIND_API_TOKEN`). */
export const HTTP_CREDENTIAL_KINDS: readonly CredentialKind[] = ['http_auth', 'api_token'];

/** Whether a credential row (whose `kind` is a free-form string server-side) may be offered as a
 *  node's SNMP binding. */
export function isSnmpCredentialKind(kind: string): boolean {
  return (SNMP_CREDENTIAL_KINDS as readonly string[]).includes(kind);
}

/** Whether a credential row may be offered as a URL monitor's authentication. */
export function isHttpCredentialKind(kind: string): boolean {
  return (HTTP_CREDENTIAL_KINDS as readonly string[]).includes(kind);
}
