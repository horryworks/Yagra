// SPDX-License-Identifier: AGPL-3.0-only
// The "type the name to confirm" judgement behind ConfirmDeleteModal's `confirmPhrase` (ADR-174).
// Here rather than in the .tsx so a test can run it (Vitest never loads a .tsx).

/** Whether what the operator typed confirms `phrase`. Surrounding whitespace is ignored on both
 *  sides — a trailing space from a paste must not keep the button dead — but case and everything
 *  inside the name must match, so a near miss on a similarly named folder does not pass.
 *
 *  🚨 An empty phrase never matches: a caller that asked for a typed confirmation and handed an
 *  empty name must not end up with a dialog that confirms on an empty box. */
export function confirmPhraseMatches(typed: string, phrase: string): boolean {
  const want = phrase.trim();
  return want !== '' && typed.trim() === want;
}
