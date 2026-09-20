// SPDX-License-Identifier: AGPL-3.0-only
// Which stored credential kinds each picker may offer.
//
// This was four hardcoded lists that had already drifted: Discovery's credential-finder offered
// `['snmp_v2c', 'snmp_v3']` while the two node-binding pickers offered `snmp_v2c` alone, so an
// SNMPv3 credential could be created, matched against a device during discovery, and then never
// bound to a node from either dialog — even though `scheduler/dispatch.rs::resolve_snmp_auth` has always
// decrypted and used one. `ui-conventions.md` names these lists as ones to fold into a source
// rather than extend; this is that source.
//
// ⚠️ The SNMP list is an **allow-list, not a convenience filter**. `resolve_snmp_auth` reads a
// v3 credential as USM parameters and treats **any other kind's bytes as a community string** —
// so offering an `http_auth` or `meraki_api` credential here would put that secret's plaintext on
// the wire in an SNMP GET. The disjointness below is a security property, and it has a test.

/** Credential kinds an operator can create in Settings ▸ Credentials.
 *
 *  Not every kind that can exist: the kinds an integration creates are in
 *  `INTEGRATION_CREDENTIAL_KINDS` below, and are deliberately absent here. */
export const CREDENTIAL_KINDS = ['snmp_v2c', 'snmp_v3', 'http_auth', 'api_token'] as const;

export type CredentialKind = (typeof CREDENTIAL_KINDS)[number];

/** Kinds an integration page creates — never Settings ▸ Credentials. They still show up there, and
 *  the edit dialog is the only way a **rotated Meraki key** gets in: no other screen replaces one. */
export const INTEGRATION_CREDENTIAL_KINDS = ['meraki_api', 'netbox_token'] as const;

export type IntegrationCredentialKind = (typeof INTEGRATION_CREDENTIAL_KINDS)[number];

/**
 * What the edit dialog's "replace the secret" may do for a stored credential.
 *
 * - `choose` — a kind created here: the type select is drawn, starting on the credential's own.
 * - `fixed` — an integration's key: the secret may be replaced, the kind may not. The select is not
 *   drawn, and the secret goes out in the shape that kind is sealed as (`integrationSecret`).
 * - `rename_only` — a kind this build does not know (a newer core wrote it). It cannot know the
 *   secret's shape either, so it offers no replacement at all.
 *
 * 🚨 The dialog used to cast any stored kind to a creatable one and draw the four-kind select over
 * it. For a Meraki key that select had no matching option, so it *showed* SNMP v2c while the state
 * said `meraki_api`: left alone, the raw key was stored where a JSON document belongs; touched, the
 * key became an SNMP community. Either way every collect and sync of the organization failed with
 * `credential` from then on, and nothing on the dialog said why (ADR-164 Inc.11).
 */
export type SecretReplacement =
  | { mode: 'choose'; initial: CredentialKind }
  | { mode: 'fixed'; kind: IntegrationCredentialKind }
  | { mode: 'rename_only' };

export function secretReplacementFor(kind: string): SecretReplacement {
  const creatable = CREDENTIAL_KINDS.find((k) => k === kind);
  if (creatable) return { mode: 'choose', initial: creatable };
  const owned = INTEGRATION_CREDENTIAL_KINDS.find((k) => k === kind);
  if (owned) return { mode: 'fixed', kind: owned };
  return { mode: 'rename_only' };
}

/**
 * The document an integration's key is sealed as, from what the operator typed.
 *
 * ⚠️ A copy of two Rust shapes — `secrets.rs::MerakiApiSecret { api_key }` and
 * `NetboxTokenSecret { token }` — which the API edge parses on the way in, so a drifted copy is a
 * `400 invalid_credential`, never a stored key nothing can read.
 * `api/credentials.rs::the_webui_seals_an_integration_key_in_the_shape_this_api_parses` reads the
 * two `JSON.stringify` lines below; keep each on one line.
 */
export function integrationSecret(kind: IntegrationCredentialKind, typed: string): string {
  const value = typed.trim();
  switch (kind) {
    case 'meraki_api':
      return JSON.stringify({ api_key: value });
    case 'netbox_token':
      return JSON.stringify({ token: value });
    default: {
      const unreachable: never = kind;
      return unreachable;
    }
  }
}

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
