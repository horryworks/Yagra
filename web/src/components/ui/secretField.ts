// SPDX-License-Identifier: AGPL-3.0-only
// What a secret field shows, and what a form sends from it (ADR-200 §5.2, `SecretInput.tsx`).
//
// A `.ts` because Vitest never loads a `.tsx` (`testing.md`). The rule the screen used to explain in
// a sentence under the box ("Leave empty to keep the stored token") is here instead, where a test
// can hold it: the field itself shows that a value is stored, and an empty box means "keep it".

/**
 * - `new` — nothing is stored: an ordinary password box.
 * - `stored` — a value is stored and the operator has not asked to change it: a masked mark, the
 *   word "Stored" and a Replace button. No box, so nothing can be typed over it by accident.
 * - `replace` — a box for the new value. `canKeep` says whether a "Keep stored" button goes back.
 */
export type SecretMode =
  | { kind: 'new' }
  | { kind: 'stored' }
  | { kind: 'replace'; canKeep: boolean };

/**
 * Which of the three the field draws.
 *
 * `mustReplace` is for a stored value that may not be used any more — a NetBox token whose address
 * now names another host (ADR-178 decision 3). The box is open and there is no way back to the
 * stored value, because the server would refuse a save that kept it.
 */
export function secretMode(stored: boolean, replacing: boolean, mustReplace = false): SecretMode {
  if (!stored) return { kind: 'new' };
  if (mustReplace) return { kind: 'replace', canKeep: false };
  return replacing ? { kind: 'replace', canKeep: true } : { kind: 'stored' };
}

/**
 * Whether the field should hold its box open from now on. True while `mustReplace` has forced it
 * open, so that when `mustReplace` turns off again (the NetBox address typed back to its old host)
 * the box stays, with "Keep stored", instead of folding back to the stored mark over a value the
 * operator typed — a value the form would still send, invisibly.
 */
export function staysReplacing(stored: boolean, replacing: boolean, mustReplace: boolean): boolean {
  return replacing || (stored && mustReplace);
}

/** What a form sends for the field: the trimmed value, or `undefined` for "keep the stored one".
 *  An empty box never clears a stored secret — clearing is a separate decision with its own
 *  control, where a screen offers one at all. */
export function secretToSend(value: string): string | undefined {
  const v = value.trim();
  return v === '' ? undefined : v;
}
