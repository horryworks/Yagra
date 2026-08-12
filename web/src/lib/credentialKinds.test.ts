// SPDX-License-Identifier: AGPL-3.0-only
// The credential-kind allow-lists. Two properties matter here, and neither is a matter of taste.

import { describe, expect, it } from 'vitest';
import {
  CREDENTIAL_KINDS,
  HTTP_CREDENTIAL_KINDS,
  SNMP_CREDENTIAL_KINDS,
  isHttpCredentialKind,
  isSnmpCredentialKind,
} from './credentialKinds';

describe('credential kind allow-lists', () => {
  it('offers both SNMP kinds for a node binding', () => {
    // The regression: the node-binding pickers filtered to `snmp_v2c` alone, so an SNMPv3
    // credential could never be bound from the UI — while `scheduler.rs::resolve_snmp_auth` reads
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
