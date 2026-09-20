// SPDX-License-Identifier: AGPL-3.0-only
// The credential-kind allow-lists. Two properties matter here, and neither is a matter of taste.

import { describe, expect, it } from 'vitest';
import {
  CREDENTIAL_KINDS,
  HTTP_CREDENTIAL_KINDS,
  INTEGRATION_CREDENTIAL_KINDS,
  SNMP_CREDENTIAL_KINDS,
  integrationSecret,
  isHttpCredentialKind,
  isSnmpCredentialKind,
  secretReplacementFor,
} from './credentialKinds';

describe('credential kind allow-lists', () => {
  it('offers both SNMP kinds for a node binding', () => {
    // The regression: the node-binding pickers filtered to `snmp_v2c` alone, so an SNMPv3
    // credential could never be bound from the UI — while `scheduler/dispatch.rs::resolve_snmp_auth` reads
    // one, `secrets.rs` stores one, and Discovery's credential finder already tried one.
    expect([...SNMP_CREDENTIAL_KINDS]).toEqual(['snmp_v2c', 'snmp_v3']);
    expect(isSnmpCredentialKind('snmp_v3')).toBe(true);
    expect(isSnmpCredentialKind('snmp_v2c')).toBe(true);
  });

  it('never offers a non-SNMP secret as an SNMP credential', () => {
    // ⚠️ Security, not tidiness. `resolve_snmp_auth` special-cases v3 and treats **every other
    // kind's bytes as a community string**, so an `http_auth` or `meraki_api` credential offered
    // here would be sent to a device in plaintext inside an SNMP GET.
    for (const kind of ['http_auth', 'api_token', 'meraki_api', '', 'snmp', 'SNMP_V2C']) {
      expect(isSnmpCredentialKind(kind), kind).toBe(false);
    }
    const overlap = SNMP_CREDENTIAL_KINDS.filter((k) => HTTP_CREDENTIAL_KINDS.includes(k));
    expect(overlap).toEqual([]);
  });

  it('offers only the kinds an HTTP probe understands', () => {
    expect([...HTTP_CREDENTIAL_KINDS]).toEqual(['http_auth', 'api_token']);
    for (const kind of ['snmp_v2c', 'snmp_v3', 'meraki_api']) {
      expect(isHttpCredentialKind(kind), kind).toBe(false);
    }
  });

  it('draws both allow-lists from the creatable set', () => {
    // A kind in a picker that Settings ▸ Credentials cannot create is a picker that can only ever
    // be empty.
    for (const k of [...SNMP_CREDENTIAL_KINDS, ...HTTP_CREDENTIAL_KINDS]) {
      expect(CREDENTIAL_KINDS, k).toContain(k);
    }
    // And every creatable kind is usable somewhere — otherwise it is a secret with no consumer.
    for (const k of CREDENTIAL_KINDS) {
      expect(isSnmpCredentialKind(k) || isHttpCredentialKind(k), k).toBe(true);
    }
  });
});

describe('replacing a stored secret', () => {
  it('lets a kind created here change, starting on its own', () => {
    for (const kind of CREDENTIAL_KINDS) {
      expect(secretReplacementFor(kind), kind).toEqual({ mode: 'choose', initial: kind });
    }
  });

  it("replaces an integration's key without ever offering another kind", () => {
    // The regression: the dialog drew the four-kind select over a Meraki key. Untouched, the raw
    // key was stored where a JSON document belongs; touched, the key became an SNMP community.
    // Both broke every collect and sync of the organization with `credential`.
    expect(secretReplacementFor('meraki_api')).toEqual({ mode: 'fixed', kind: 'meraki_api' });
    expect(secretReplacementFor('netbox_token')).toEqual({ mode: 'fixed', kind: 'netbox_token' });
    // Neither list may hold the other's kinds, or one credential would get two answers.
    const both = INTEGRATION_CREDENTIAL_KINDS.filter((k) =>
      (CREDENTIAL_KINDS as readonly string[]).includes(k),
    );
    expect(both).toEqual([]);
  });

  it('offers no replacement for a kind this build has never heard of', () => {
    // A newer core can store one. This build cannot know the shape its secret is sealed in.
    for (const kind of ['snmp_v4', '', 'MERAKI_API', 'meraki']) {
      expect(secretReplacementFor(kind), kind).toEqual({ mode: 'rename_only' });
    }
  });

  it("seals an integration's key as the document the server parses", () => {
    expect(JSON.parse(integrationSecret('meraki_api', 'abc123'))).toEqual({ api_key: 'abc123' });
    expect(JSON.parse(integrationSecret('netbox_token', 'tok'))).toEqual({ token: 'tok' });
    // Pasted with the line break the Dashboard's copy button leaves behind.
    expect(JSON.parse(integrationSecret('meraki_api', '  abc123\n'))).toEqual({ api_key: 'abc123' });
    // A key is quoted, never spliced: one holding a quote is still one JSON string.
    expect(JSON.parse(integrationSecret('meraki_api', 'a"b\\c'))).toEqual({ api_key: 'a"b\\c' });
  });
});
