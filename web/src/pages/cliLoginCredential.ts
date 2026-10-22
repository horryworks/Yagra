// SPDX-License-Identifier: AGPL-3.0-only
// The CLI login credential form's judgement (ADR-210 stage 2): which fields are required, and the
// JSON document the backend seals.
//
// Out of CredentialsPage.tsx so a test can reach it — Vitest never runs `.tsx`. The authority is
// `crates/yagra-base/src/secrets.rs::CliLoginSecret::parse`; checking here keeps Save disabled
// until the dialog holds what the server will accept.

/** The sub-form's fields. */
export interface CliLoginState {
  username: string;
  password: string;
  /** The privileged-mode password; empty means the account lands privileged. */
  enable: string;
}

export const emptyCliLogin = (): CliLoginState => ({ username: '', password: '', enable: '' });

/** Whether the server will accept the document: a user name and a password. */
export function cliLoginReady(s: CliLoginState): boolean {
  return s.username.trim() !== '' && s.password !== '';
}

/** The sealed document. An empty enable is left out rather than sent blank. */
export function buildCliLoginSecret(s: CliLoginState): string {
  const doc: { username: string; password: string; enable?: string } = {
    username: s.username.trim(),
    password: s.password,
  };
  if (s.enable !== '') doc.enable = s.enable;
  return JSON.stringify(doc);
}
