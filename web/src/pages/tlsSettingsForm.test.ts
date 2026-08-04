// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';

import {
  certificateFilename,
  expiryLevel,
  importBlock,
  parseNames,
  redirectUriMismatch,
} from './tlsSettingsForm';

const CERT = '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n';
const KEY = '-----BEGIN PRIVATE KEY-----\nMIIB\n-----END PRIVATE KEY-----\n';

describe('expiryLevel', () => {
  it('escalates as the date approaches', () => {
    expect(expiryLevel(365)).toBe('ok');
    expect(expiryLevel(31)).toBe('ok');
    expect(expiryLevel(30)).toBe('soon');
    expect(expiryLevel(8)).toBe('soon');
    expect(expiryLevel(7)).toBe('critical');
    expect(expiryLevel(0)).toBe('critical');
    expect(expiryLevel(-1)).toBe('expired');
  });

  it('starts warning at the same point the backend starts renewing', () => {
    // If these drifted apart, the page would either nag about a certificate that is already fixing
    // itself, or stay quiet through the window in which renewal could still fail.
    expect(expiryLevel(30)).toBe('soon');
  });
});

describe('importBlock', () => {
  it('accepts a well-formed pair', () => {
    expect(importBlock(CERT, KEY)).toBeNull();
  });

  it('names each missing or malformed half', () => {
    expect(importBlock('', KEY)).toBe('certificate-missing');
    expect(importBlock('   \n', KEY)).toBe('certificate-missing');
    expect(importBlock('hello', KEY)).toBe('certificate-not-pem');
    expect(importBlock(CERT, '')).toBe('key-missing');
    expect(importBlock(CERT, 'hello')).toBe('key-not-pem');
  });

  it('refuses a private key pasted into the certificate box', () => {
    // The certificate is stored in the clear and offered for download, so this one is not a
    // convenience check — it is the difference between a key staying private and being published.
    expect(importBlock(CERT + KEY, KEY)).toBe('certificate-has-key');
  });

  it('recognises an encrypted key rather than calling it malformed', () => {
    // "not PEM" would send the operator looking for a corrupt file. The real answer is that it needs
    // converting, and the UI can only say so if it can tell the two apart.
    const encrypted =
      '-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIB\n-----END ENCRYPTED PRIVATE KEY-----\n';
    expect(importBlock(CERT, encrypted)).toBe('key-encrypted');
  });

  it('accepts the PKCS#1 and SEC1 key labels too', () => {
    for (const label of ['RSA PRIVATE KEY', 'EC PRIVATE KEY']) {
      const k = `-----BEGIN ${label}-----\nMIIB\n-----END ${label}-----\n`;
      expect(importBlock(CERT, k)).toBeNull();
    }
  });
});

describe('parseNames', () => {
  it('splits on commas and newlines, trims, and keeps first-seen order', () => {
    expect(parseNames('yagra.example.net, 10.0.0.5\n  localhost ')).toEqual([
      'yagra.example.net',
      '10.0.0.5',
      'localhost',
    ]);
  });

  it('drops blanks and duplicates', () => {
    expect(parseNames('a,,a\n\n b ,a')).toEqual(['a', 'b']);
    expect(parseNames('   ')).toEqual([]);
  });
});

describe('redirectUriMismatch', () => {
  it('flags the exact change this ADR causes', () => {
    expect(redirectUriMismatch('https://yagra.example.net', 'http://yagra.example.net:3000/auth/callback')).toBe(
      true,
    );
  });

  it('is quiet when only the path differs', () => {
    // The path is the operator's choice and this upgrade did not touch it; warning about it would
    // train them to ignore the banner that matters.
    expect(
      redirectUriMismatch('https://yagra.example.net/settings/auth', 'https://yagra.example.net/auth/callback'),
    ).toBe(false);
  });

  it('is quiet with nothing stored, and does not guess at unparseable values', () => {
    expect(redirectUriMismatch('https://h', null)).toBe(false);
    expect(redirectUriMismatch('https://h', 'not a url')).toBe(false);
  });

  it('treats a differing port as a mismatch', () => {
    expect(redirectUriMismatch('https://h', 'https://h:3000/cb')).toBe(true);
  });
});

describe('certificateFilename', () => {
  it('uses the first name and strips anything unsafe for a filename', () => {
    expect(certificateFilename({ sans: ['yagra.example.net'] })).toBe('yagra.example.net.crt');
    expect(certificateFilename({ sans: ['*.example.net'] })).toBe('_.example.net.crt');
    expect(certificateFilename({ sans: [] })).toBe('yagra.crt');
  });
});
