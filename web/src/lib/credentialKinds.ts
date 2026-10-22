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

/** Whether a credential's secret can be read back on screen (ADR-210): the device kinds an
 *  operator creates, never an integration's key and never a kind this build does not know.
 *  `api/credentials.rs::revealed_fields` answers the same set, and a Rust test reads
 *  `CREDENTIAL_KINDS` above to keep the two equal. */
export function isRevealableCredentialKind(kind: string): boolean {
  return (CREDENTIAL_KINDS as readonly string[]).includes(kind);
}

/** How long a revealed secret stays on screen before its dialog closes by itself (ADR-210). */
export const REVEAL_AUTO_CLOSE_MS = 60_000;

/** The tabs of Nodes ▸ Credentials & secrets (ADR-205 Inc.1), in the order they are drawn. The
 *  first is what a bare URL opens. `other` holds a kind this build does not know — a newer core can
 *  store one — and is drawn only while such a row exists, so no row is ever silently left out. */
export const CREDENTIAL_GROUPS = ['snmp', 'http', 'integration', 'other'] as const;

export type CredentialGroup = (typeof CREDENTIAL_GROUPS)[number];

/** The kinds each group holds, built from the allow-lists above rather than listed again.
 *
 *  ⚠️ `api_token` is in `http`, not `integration`: despite its name it is the bearer token a URL
 *  monitor sends (`secrets.rs::KIND_API_TOKEN`), so it sits with `http_auth`. */
export const CREDENTIAL_GROUP_KINDS: Record<Exclude<CredentialGroup, 'other'>, readonly string[]> = {
  snmp: SNMP_CREDENTIAL_KINDS,
  http: HTTP_CREDENTIAL_KINDS,
  integration: INTEGRATION_CREDENTIAL_KINDS,
};

/** Which tab a stored credential is listed under: looked up in `CREDENTIAL_GROUP_KINDS`, so the
 *  table above is the one answer, and `other` for a kind it does not hold. */
export function credentialGroupOf(kind: string): CredentialGroup {
  const groups = Object.keys(CREDENTIAL_GROUP_KINDS) as Exclude<CredentialGroup, 'other'>[];
  return groups.find((g) => CREDENTIAL_GROUP_KINDS[g].includes(kind)) ?? 'other';
}

/** The kinds the "+ Add" dialog offers on a tab: the creatable kinds of that group, in
 *  `CREDENTIAL_KINDS` order. Empty for `integration` (those keys are created on the integration
 *  pages) and for `other`. */
export function creatableKindsIn(group: CredentialGroup): CredentialKind[] {
  return CREDENTIAL_KINDS.filter((k) => credentialGroupOf(k) === group);
}
