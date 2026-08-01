// SPDX-License-Identifier: AGPL-3.0-only
// The SNMPv3 credential form's completeness rule and its USM serialization. Both decide what gets
// encrypted at rest and then used to poll a device, so a mistake here is a credential that looks
// saved and fails on every poll.

import { describe, expect, it } from 'vitest';
import {
  buildV3Secret,
  emptyV3,
  v3Ready,
  V3_AUTH_PROTOCOLS,
  V3_LEVELS,
  V3_PRIV_PROTOCOLS,
  type V3State,
} from './v3Credential';

const v3 = (over: Partial<V3State> = {}): V3State => ({
  ...emptyV3(),
  user: 'monitor',
  authKey: 'authsecret',
  privKey: 'privsecret',
  ...over,
});

describe('v3Ready', () => {
  it('always requires a username, whatever the level', () => {
    for (const level of V3_LEVELS) {
      expect(v3Ready(v3({ level, user: '' }))).toBe(false);
      expect(v3Ready(v3({ level, user: '   ' }))).toBe(false);
    }
  });

  it('requires both keys at authpriv', () => {
    expect(v3Ready(v3({ level: 'authpriv' }))).toBe(true);
    expect(v3Ready(v3({ level: 'authpriv', authKey: '' }))).toBe(false);
    expect(v3Ready(v3({ level: 'authpriv', privKey: '' }))).toBe(false);
  });

  it('requires only the auth key at auth', () => {
    expect(v3Ready(v3({ level: 'auth', privKey: '' }))).toBe(true);
    expect(v3Ready(v3({ level: 'auth', authKey: '' }))).toBe(false);
  });

  it('requires neither key at noauth', () => {
    expect(v3Ready(v3({ level: 'noauth', authKey: '', privKey: '' }))).toBe(true);
  });

  it('rejects a fresh form', () => {
    expect(v3Ready(emptyV3())).toBe(false);
  });
});

describe('buildV3Secret', () => {
  it('omits the fields the level does not use rather than blanking them', () => {
    // `auth_key: ""` would be stored and sealed as an empty secret that reads as configured —
    // the credential would look complete and authenticate against nothing.
    const noauth = JSON.parse(buildV3Secret(v3({ level: 'noauth' })));
    expect(noauth).toEqual({ user: 'monitor', security_level: 'noauth' });
    expect('auth_key' in noauth).toBe(false);
    expect('priv_key' in noauth).toBe(false);

    const auth = JSON.parse(buildV3Secret(v3({ level: 'auth' })));
    expect(auth).toMatchObject({ security_level: 'auth', auth_key: 'authsecret' });
    expect('priv_key' in auth).toBe(false);
    expect('priv_protocol' in auth).toBe(false);
  });

  it('carries both key pairs at authpriv', () => {
    const doc = JSON.parse(
      buildV3Secret(
        v3({ level: 'authpriv', authProto: 'sha256', privProto: 'aes256' }),
      ),
    );
    expect(doc).toEqual({
      user: 'monitor',
      security_level: 'authpriv',
      auth_protocol: 'sha256',
      auth_key: 'authsecret',
      priv_protocol: 'aes256',
      priv_key: 'privsecret',
    });
  });

  it('trims the username so a stray space is not part of the USM identity', () => {
    // USM matches the user name byte-for-byte on the device; " monitor" is a different user.
    expect(JSON.parse(buildV3Secret(v3({ user: '  monitor  ' }))).user).toBe('monitor');
  });

  it('defaults to the first offered protocol of each kind', () => {
    const doc = JSON.parse(buildV3Secret(v3()));
    expect(doc.auth_protocol).toBe(V3_AUTH_PROTOCOLS[0]);
    expect(doc.priv_protocol).toBe(V3_PRIV_PROTOCOLS[0]);
  });

  it('offers no protocol twice, so the pickers cannot show a duplicate', () => {
    expect(new Set(V3_AUTH_PROTOCOLS).size).toBe(V3_AUTH_PROTOCOLS.length);
    expect(new Set(V3_PRIV_PROTOCOLS).size).toBe(V3_PRIV_PROTOCOLS.length);
  });
});
