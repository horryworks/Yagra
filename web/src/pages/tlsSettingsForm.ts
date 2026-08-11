// SPDX-License-Identifier: AGPL-3.0-only
// Judgement for Settings ▸ TLS (ADR-044), kept out of the .tsx so it can be tested.
//
// Vitest runs with `environment: 'node'` and `include: ['src/**/*.test.ts']`, so a test written
// beside the component would never execute. Everything here decides something; the page decides
// only layout.

import type { WebTlsView } from '../types/api';

/** How urgently the operator should care about the expiry date. */
export const EXPIRY_LEVELS = ['ok', 'soon', 'critical', 'expired'] as const;
export type ExpiryLevel = (typeof EXPIRY_LEVELS)[number];

/** Thresholds in days. `soon` matches the backend's own renewal window, so a self-signed
 *  certificate starts renewing itself at the moment the page starts saying "soon". */
const SOON_DAYS = 30;
const CRITICAL_DAYS = 7;

export function expiryLevel(expiresInDays: number): ExpiryLevel {
  if (expiresInDays < 0) return 'expired';
  if (expiresInDays <= CRITICAL_DAYS) return 'critical';
  if (expiresInDays <= SOON_DAYS) return 'soon';
  return 'ok';
}

/** Every reason the import button can be disabled. `as const` so `i18nEnumKeys.test.ts` can walk it
 *  — the page renders `t(`import.block.${block}`)` with no fallback, so a reason added without its
 *  strings shows the operator a raw key in both locales while they are replacing the certificate
 *  their browser trusts. */
export const IMPORT_BLOCKS = [
  'certificate-missing',
  'certificate-not-pem',
  'certificate-has-key',
  'key-missing',
  'key-not-pem',
  'key-encrypted',
] as const;

/** Why the import button is disabled, or `null` when it is not. */
export type ImportBlock = (typeof IMPORT_BLOCKS)[number];

const CERT_HEADER = '-----BEGIN CERTIFICATE-----';
const KEY_HEADER = /-----BEGIN (RSA |EC )?PRIVATE KEY-----/;
const ENCRYPTED_KEY_HEADER = '-----BEGIN ENCRYPTED PRIVATE KEY-----';

/**
 * Client-side pre-checks on the pasted PEM.
 *
 * Deliberately shallow — the server does the real validation, and this cannot verify that a key
 * matches a certificate. What it buys is the same thing `configBundle.ts` buys: an operator who
 * pasted the wrong file finds out from the form rather than from a round trip. Every rule here has a
 * server-side counterpart that would refuse the same input, so this can only be early, never
 * authoritative.
 */
export function importBlock(certificate: string, privateKey: string): ImportBlock | null {
  const cert = certificate.trim();
  const key = privateKey.trim();
  if (!cert) return 'certificate-missing';
  if (!cert.includes(CERT_HEADER)) return 'certificate-not-pem';
  // Caught here as well as on the server because of what it costs to get wrong: the certificate is
  // stored in the clear and offered for download, so a key pasted into this box would be published.
  if (KEY_HEADER.test(cert)) return 'certificate-has-key';
  if (!key) return 'key-missing';
  if (key.includes(ENCRYPTED_KEY_HEADER)) return 'key-encrypted';
  if (!KEY_HEADER.test(key)) return 'key-not-pem';
  return null;
}

/** Names for the regenerate form: comma or newline separated, trimmed, de-duplicated, order kept. */
export function parseNames(raw: string): string[] {
  const seen = new Set<string>();
  return raw
    .split(/[\n,]/)
    .map((n) => n.trim())
    .filter((n) => n.length > 0 && !seen.has(n) && seen.add(n));
}

/**
 * Whether a stored OIDC redirect URI still matches the address the browser is on.
 *
 * ADR-044 changes the scheme and usually the port, and `oidc_providers.redirect_uri` is an absolute
 * URL that has to agree with what is registered at the identity provider. Getting it wrong fails at
 * the token exchange, which reads to the operator as "SSO is broken" with nothing pointing at the
 * upgrade. A banner turns that into a sentence.
 *
 * Compares origins only: the path is the operator's to choose, and a difference there is not
 * something this upgrade caused. Returns `false` for anything unparseable — an unreadable stored
 * value is a different problem, and guessing would produce a warning nobody can act on.
 */
export function redirectUriMismatch(currentOrigin: string, storedUri: string | null): boolean {
  if (!storedUri) return false;
  try {
    return new URL(storedUri).origin !== new URL(currentOrigin).origin;
  } catch {
    return false;
  }
}

/** A filename for the downloaded certificate, derived from the first SAN. */
export function certificateFilename(view: Pick<WebTlsView, 'sans'>): string {
  const name = view.sans[0]?.replace(/[^a-zA-Z0-9._-]/g, '_') ?? 'yagra';
  return `${name || 'yagra'}.crt`;
}
