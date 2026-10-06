// SPDX-License-Identifier: AGPL-3.0-only
// Text measured the way core measures it, for a screen that previews a rule core enforces.
//
// JavaScript and Rust disagree on three everyday-looking operations: `String.prototype.trim`
// strips U+FEFF and keeps U+0085, while Rust's `str::trim` does the opposite (it strips exactly
// the Unicode `White_Space` property); `.length` counts UTF-16 units where Rust's `len()` counts
// UTF-8 bytes; and `toLowerCase()` folds the whole of Unicode where `eq_ignore_ascii_case` folds
// only `A`-`Z`. A preview that uses the JavaScript ones answers differently from the server on
// those inputs.

const EDGE_WHITESPACE = /^\p{White_Space}+|\p{White_Space}+$/gu;

/** Rust's `str::trim`: strip leading and trailing `White_Space` characters, and nothing else. */
export function trimLikeCore(s: string): string {
  return s.replace(EDGE_WHITESPACE, '');
}

/** Rust's `str::len`: the length in UTF-8 bytes. */
export function utf8Length(s: string): number {
  return new TextEncoder().encode(s).length;
}

/** Rust's `str::eq_ignore_ascii_case`: fold `A`–`Z` and leave every other code point alone.
 *
 *  JavaScript's `toLowerCase()` is a full Unicode fold and answers `true` where core answers
 *  `false` — `K` (U+212A) folds to `k`, `İ` grows a combining dot — so a screen that compares with
 *  it reports a value as accepted that the server then refuses. */
export function eqIgnoreAsciiCase(a: string, b: string): boolean {
  return foldAscii(a) === foldAscii(b);
}

const ASCII_UPPER = /[A-Z]/g;

function foldAscii(s: string): string {
  return s.replace(ASCII_UPPER, (c) => c.toLowerCase());
}
