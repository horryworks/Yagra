// SPDX-License-Identifier: AGPL-3.0-only
// The SNMPv3 (USM) credential form's judgement: which keys a security level requires, and the
// exact JSON document the backend seals.
//
// Extracted from CredentialsPage.tsx so it is testable at all — Vitest never runs `.tsx`. It is
// worth reaching: this decides whether a monitoring credential is complete before it is encrypted
// at rest, and an under-specified one produces a v3 session that fails on every poll of the device
// it was created for.

/** The three USM security levels, weakest last. */
export const V3_LEVELS = ['authpriv', 'auth', 'noauth'] as const;
export type V3Level = (typeof V3_LEVELS)[number];

/** Authentication digests the backend accepts, strongest-typical first. */
export const V3_AUTH_PROTOCOLS = ['sha', 'sha224', 'sha256', 'sha384', 'sha512', 'md5'];
/** Privacy ciphers the backend accepts. */
export const V3_PRIV_PROTOCOLS = ['aes', 'aes192', 'aes256', 'des'];

/** The v3 sub-form's fields. Every key is held even when the level does not use it, so switching
 *  level back and forth does not lose what was typed. */
export interface V3State {
  user: string;
  level: V3Level;
  authProto: string;
  authKey: string;
  privProto: string;
  privKey: string;
}

export const emptyV3 = (): V3State => ({
  user: '',
  level: 'authpriv',
  authProto: V3_AUTH_PROTOCOLS[0],
  authKey: '',
  privProto: V3_PRIV_PROTOCOLS[0],
  privKey: '',
});

/** Which key fields the declared level makes mandatory. `authpriv` needs both, `auth` only the
 *  digest, `noauth` neither — the one place that mapping is written. */
function requirements(level: V3Level): { auth: boolean; priv: boolean } {
  return { auth: level !== 'noauth', priv: level === 'authpriv' };
}

/** Whether the v3 form has the keys its declared security level requires. */
export function v3Ready(v: V3State): boolean {
  const need = requirements(v.level);
  return (
    v.user.trim() !== '' && (!need.auth || v.authKey !== '') && (!need.priv || v.privKey !== '')
  );
}

/** Serialize the v3 form into the USM JSON document the backend validates and seals.
 *
 *  Fields the level does not use are **omitted, not blanked**: sending `auth_key: ""` for a
 *  `noauth` credential would store an empty secret that reads as configured. */
export function buildV3Secret(v: V3State): string {
  const need = requirements(v.level);
  return JSON.stringify({
    user: v.user.trim(),
    security_level: v.level,
    ...(need.auth ? { auth_protocol: v.authProto, auth_key: v.authKey } : {}),
    ...(need.priv ? { priv_protocol: v.privProto, priv_key: v.privKey } : {}),
  });
}
