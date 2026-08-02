// SPDX-License-Identifier: AGPL-3.0-only
// The HTTP-auth credential form's judgement: which fields a scheme requires, and the exact JSON
// document the backend seals.
//
// Extracted from CredentialsPage.tsx so it is testable at all — Vitest never runs `.tsx`. The
// mirror of this validation lives in `crates/yagra-core/src/secrets.rs::parse_http_auth`, which is
// the authority; checking here turns a round trip into an inline message, and keeps an
// under-specified credential from being encrypted at rest in the first place.

/** The schemes a URL monitor can present. Mirrors `yagra_common::HTTP_AUTH_SCHEMES`. */
export const HTTP_AUTH_SCHEMES = ['basic', 'bearer', 'header'] as const;
export type HttpAuthScheme = (typeof HTTP_AUTH_SCHEMES)[number];

/** The sub-form's fields. Every key is held even when the scheme does not use it, so switching
 *  scheme back and forth does not lose what was typed. */
export interface HttpAuthState {
  scheme: HttpAuthScheme;
  username: string;
  password: string;
  token: string;
  headerName: string;
  headerValue: string;
}

export const emptyHttpAuth = (): HttpAuthState => ({
  scheme: 'basic',
  username: '',
  password: '',
  token: '',
  headerName: '',
  headerValue: '',
});

/** Header names the probe owns, which a credential may not override.
 *
 *  `Host` retargets the request, `Authorization` collides with the Basic/Bearer schemes, and the
 *  rest are hop-by-hop headers belonging to the connection. Kept in sync with `RESERVED` in
 *  `secrets.rs`, which is the enforcing copy. */
const RESERVED_HEADERS = [
  'host',
  'authorization',
  'connection',
  'content-length',
  'transfer-encoding',
  'upgrade',
  'te',
  'trailer',
];

/** Whether `name` is an RFC 7230 token this credential is allowed to set. */
export function isValidHeaderName(name: string): boolean {
  if (name === '' || name.length > 64) return false;
  if (RESERVED_HEADERS.includes(name.toLowerCase())) return false;
  return /^[A-Za-z0-9!#$%&'*+\-.^_`|~]+$/.test(name);
}

/** Whether the sub-form has everything its scheme needs. */
export function httpAuthReady(s: HttpAuthState): boolean {
  switch (s.scheme) {
    case 'basic':
      return s.username.trim() !== '' && s.password !== '';
    case 'bearer':
      return s.token.trim() !== '';
    case 'header':
      return isValidHeaderName(s.headerName.trim()) && s.headerValue.trim() !== '';
  }
}

/** The sealed document, exactly as `parse_http_auth` expects to read it back. */
export function buildHttpAuthSecret(s: HttpAuthState): string {
  switch (s.scheme) {
    case 'basic':
      return JSON.stringify({
        scheme: 'basic',
        username: s.username.trim(),
        password: s.password,
      });
    case 'bearer':
      return JSON.stringify({ scheme: 'bearer', token: s.token.trim() });
    case 'header':
      return JSON.stringify({
        scheme: 'header',
        name: s.headerName.trim(),
        value: s.headerValue,
      });
  }
}
