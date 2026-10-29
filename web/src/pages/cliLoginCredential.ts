// SPDX-License-Identifier: AGPL-3.0-only
// The CLI login credential form's judgement (ADR-210 stage 2): which fields are required, and the
// JSON document the backend seals.
//
// Out of CredentialsPage.tsx so a test can reach it — Vitest never runs `.tsx`. The authority is
// `crates/yagra-base/src/secrets.rs::CliLoginSecret::parse`; checking here keeps Save disabled
// until the dialog holds what the server will accept.

/** The sub-form's fields. */
export interface CliLoginState {
  /** Empty for a device whose lines check a password alone. */
  username: string;
  password: string;
  /** The privileged-mode password; empty means the account lands privileged. */
  enable: string;
}

export const emptyCliLogin = (): CliLoginState => ({ username: '', password: '', enable: '' });

/** Whether the server will accept the document: a password, with or without a user name. */
export function cliLoginReady(s: CliLoginState): boolean {
  return s.password !== '';
}

/**
 * The sealed document. A blank user name is sent as `""`, never left out, so oxidized is handed
 * an empty one rather than its own config's default. An empty enable is left out.
 */
export function buildCliLoginSecret(s: CliLoginState): string {
  const doc: { username: string; password: string; enable?: string } = {
    username: s.username.trim(),
    password: s.password,
  };
  if (s.enable !== '') doc.enable = s.enable;
  return JSON.stringify(doc);
}
