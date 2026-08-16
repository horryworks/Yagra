// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { BusTlsView } from '../types/api';
import {
  BUS_CERT_WARN_DAYS,
  busCertState,
  busPollerEnv,
  coversName,
  namesNotCovered,
  parseBusNames,
} from './busCert';

function cert(over: Partial<BusTlsView> = {}): BusTlsView {
  return {
    certificate: '-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----\n',
    subject: 'CN=nats',
    issuer: 'CN=nats',
    sans: ['nats', 'localhost', '127.0.0.1'],
    not_before: '2026-01-01T00:00:00Z',
    not_after: '2027-02-01T00:00:00Z',
    fingerprint_sha256: 'ab'.repeat(32),
    key_algorithm: 'ECDSA P-256',
    expires_in_days: 300,
    issued_at: '2026-01-01T00:00:00Z',
    issued_by: null,
    materialized: true,
    key_unreadable: false,
    ...over,
  };
}

describe('busCertState', () => {
  it('reports one state, worst first', () => {
    // The ordering is the assertion. A certificate that is expired AND unmaterialized is one
    // problem to an operator, and showing the milder of the two would be actively misleading.
    expect(busCertState(null)).toBe('absent');
    expect(busCertState(cert({ key_unreadable: true, expires_in_days: -5 }))).toBe('unreadable');
    expect(busCertState(cert({ expires_in_days: -1, materialized: false }))).toBe('expired');
    expect(busCertState(cert({ materialized: false }))).toBe('not_materialized');
    expect(busCertState(cert({ expires_in_days: BUS_CERT_WARN_DAYS }))).toBe('expiring');
    expect(busCertState(cert())).toBe('ok');
  });

  it('warns with enough time to visit the sites', () => {
    // Not a style choice: reissuing means handing a new file to every remote poller, so the warning
    // has to arrive before a maintenance window has to be booked. Day 0 still counts as expiring.
    expect(BUS_CERT_WARN_DAYS).toBeGreaterThan(30);
    expect(busCertState(cert({ expires_in_days: 0 }))).toBe('expiring');
    expect(busCertState(cert({ expires_in_days: BUS_CERT_WARN_DAYS + 1 }))).toBe('ok');
  });
});

describe('parseBusNames', () => {
  it('takes commas, spaces and newlines, because all three get pasted', () => {
    expect(parseBusNames('a.example.net, b.example.net\n 203.0.113.10')).toEqual([
      'a.example.net',
      'b.example.net',
      '203.0.113.10',
    ]);
  });

  it('deduplicates case-insensitively and drops blanks', () => {
    // Two SANs differing only in case are one certificate entry pretending to be two, which makes
    // the "not covered" list disagree with the certificate it describes.
    expect(parseBusNames('A.example.net, a.example.net,,  ')).toEqual(['A.example.net']);
    expect(parseBusNames('   ')).toEqual([]);
  });
});

describe('coversName', () => {
  it('matches case-insensitively', () => {
    expect(coversName(cert({ sans: ['Yagra.Example.Net'] }), 'yagra.example.net')).toBe(true);
  });

  it('never treats a wildcard as covering anything', () => {
    // The permissive direction is the dangerous one: it would tell an operator a site can connect
    // and send them there to debug a handshake this page promised would work. Generation never
    // emits a wildcard SAN, so there is nothing legitimate to match here.
    expect(coversName(cert({ sans: ['*.example.net'] }), 'a.example.net')).toBe(false);
    expect(coversName(cert({ sans: ['*.example.net'] }), '*.example.net')).toBe(true);
  });

  it('is false for no certificate and for an empty name', () => {
    expect(coversName(null, 'nats')).toBe(false);
    expect(coversName(cert(), '   ')).toBe(false);
  });
});

describe('namesNotCovered', () => {
  it('names exactly what an operator has to reissue for', () => {
    const missing = namesNotCovered(cert(), ['nats', 'yagra.example.net', '127.0.0.1']);
    expect(missing).toEqual(['yagra.example.net']);
    expect(namesNotCovered(cert(), ['nats'])).toEqual([]);
  });
});

describe('busPollerEnv', () => {
  it('carries the secret, the CA path and the TLS scheme', () => {
    // The one artifact the operator carries off this screen, and the secret is shown once — a wrong
    // line here has no second chance to be noticed before the site is configured.
    const env = busPollerEnv({
      pollerId: 'site-a',
      pool: 'site-a',
      host: 'yagra.example.net',
      secret: 'S3cretSecretSecret',
    });
    expect(env).toContain('YAGRA_POLLER_ID=site-a');
    expect(env).toContain('YAGRA_BUS_URL=tls://poller:S3cretSecretSecret@yagra.example.net:4222');
    expect(env).toContain('YAGRA_BUS_CA_FILE=/etc/nats/certs/server-cert.pem');
    // Plaintext would put device credentials on the WAN in the clear (ADR-020).
    expect(env).not.toContain('nats://');
  });

  it('honours a non-default port', () => {
    const env = busPollerEnv({
      pollerId: 'p',
      pool: 'default',
      host: 'h',
      port: 14222,
      secret: 'abcdefghijklmnop',
    });
    expect(env).toContain('@h:14222');
  });
});
