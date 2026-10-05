// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { baseUrlRefused, pemIsPrivateKey } from './netboxForm';

describe('baseUrlRefused', () => {
  it('refuses the addresses the backend refuses', () => {
    for (const url of [
      'http://127.0.0.1',
      'http://127.1:8000',
      'http://0x7f000001/',
      'https://169.254.169.254/latest',
      'http://0.0.0.0',
      'http://224.0.0.1',
      'http://255.255.255.255',
      'http://[::1]:8000',
      'http://[::]',
      'http://[fe80::1]',
      'http://[ff02::1]',
      'http://[::ffff:127.0.0.1]',
      'http://[::ffff:169.254.169.254]',
    ]) {
      expect(baseUrlRefused(url), url).toBe(true);
    }
  });

  it('accepts private addresses, hostnames and public addresses', () => {
    for (const url of [
      'https://netbox.example.com',
      'http://10.0.0.1:8000',
      'http://192.168.1.10',
      'http://192.0.2.10/netbox',
      'http://[2001:db8::1]',
      'http://[fd00::1]',
      'http://[::ffff:192.0.2.1]',
      // A hostname is never resolved here — the backend does not resolve it either.
      'http://localhost:8000',
    ]) {
      expect(baseUrlRefused(url), url).toBe(false);
    }
  });

  it('leaves what does not parse to the backend', () => {
    expect(baseUrlRefused('')).toBe(false);
    expect(baseUrlRefused('not a url')).toBe(false);
    expect(baseUrlRefused('netbox.example.com')).toBe(false);
  });
});

describe('pemIsPrivateKey', () => {
  it('recognises the key spellings', () => {
    expect(pemIsPrivateKey('-----BEGIN PRIVATE KEY-----\nMII…')).toBe(true);
    expect(pemIsPrivateKey('-----BEGIN RSA PRIVATE KEY-----')).toBe(true);
    expect(pemIsPrivateKey('-----BEGIN EC PRIVATE KEY-----')).toBe(true);
    expect(pemIsPrivateKey('-----BEGIN ENCRYPTED PRIVATE KEY-----')).toBe(true);
  });

  it('accepts a certificate and an empty box', () => {
    expect(pemIsPrivateKey('-----BEGIN CERTIFICATE-----\nMII…\n-----END CERTIFICATE-----')).toBe(
      false,
    );
    expect(pemIsPrivateKey('')).toBe(false);
  });
});
