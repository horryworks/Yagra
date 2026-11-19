// SPDX-License-Identifier: AGPL-3.0-only
/** How the inventory search box's term is read (ADR-258): words separated by white space, every
 *  one of which has to match.
 *
 *  🚨 **A mirror of `yagra_common::node_search`.** Core decides which NODES match; this copy only
 *  decides which FOLDERS the tree shows whole (every word somewhere on the folder's path). The two
 *  are pinned to one case table, `fixtures/searchWords.cases.json`, run by a test on each side —
 *  a new case goes there, never into one test. */

/** How many words one search may hold. Core refuses a ninth with 400 `too_many_terms`. */
export const MAX_SEARCH_WORDS = 8;

/** Unicode `White_Space`, which is what Rust's `char::is_whitespace` splits on. Not `\s`: that one
 *  also takes U+FEFF and leaves out U+0085, so the two sides would disagree on a pasted term. */
const WHITE_SPACE = /[\t\n\v\f\r \u0085\u00a0\u1680\u2000-\u200a\u2028\u2029\u202f\u205f\u3000]+/u;

/** The words of `term`, lower-cased, in the order typed, without repeats; `null` past
 *  {@link MAX_SEARCH_WORDS}. A blank term is no words, which means "not searching". */
export function searchWords(term: string): string[] | null {
  const out: string[] = [];
  for (const raw of term.split(WHITE_SPACE)) {
    if (raw === '') continue;
    const word = raw.toLowerCase();
    if (!out.includes(word)) out.push(word);
  }
  return out.length > MAX_SEARCH_WORDS ? null : out;
}

/** Whether every word is found in at least one of `names` (a folder and the folders above it).
 *  No words is never a match. */
export function everyWordIn(words: readonly string[], names: readonly string[]): boolean {
  if (words.length === 0) return false;
  const lowered = names.map((n) => n.toLowerCase());
  return words.every((w) => lowered.some((n) => n.includes(w)));
}

/** Whether a word could be part of an IP address — the words core also compares with a port's
 *  addresses. */
export function looksLikeAddressPart(word: string): boolean {
  return (
    /^[0-9a-fA-F.:]+$/.test(word) &&
    (word.includes('.') || word.includes(':') || /^[0-9]+$/.test(word))
  );
}
