// SPDX-License-Identifier: AGPL-3.0-only
// Text measured the way core measures it, for a screen that previews a rule core enforces.
//
// JavaScript and Rust disagree on two everyday-looking operations: `String.prototype.trim` strips
// U+FEFF and keeps U+0085, while Rust's `str::trim` does the opposite (it strips exactly the
// Unicode `White_Space` property); and `.length` counts UTF-16 units where Rust's `len()` counts
// UTF-8 bytes. A preview that uses the JavaScript ones answers differently from the server on
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
