// SPDX-License-Identifier: AGPL-3.0-only
// The text condition a column filter carries, and its single-string codec (ADR-053 decision 12).
//
// A text filter has three dimensions — term, mode, negation — and they travel in ONE query-string
// key (`message=!~^%LINK-3`), not three. The reason is the rule that makes the URL readable at all:
// "at its default ⇒ the key is deleted, so a query string means something is narrowing this". With
// three keys, clearing one filter is three deletes and any missed one leaves `message_mode=regex`
// stranded in the URL of an unfiltered view.
//
// The price is an escape rule, which is exactly the kind of thing that ships subtly wrong. So the
// codec lives here, in a `.ts` where tests actually run (`.tsx` tests are never executed — see
// .claude/rules/testing.md), and carries a round-trip property test over the awkward terms:
// `!link`, `~link`, `\link`, and a term that is only whitespace.

import type { TextMode } from './columnFilter';

export interface TextCondition {
  term: string;
  mode: TextMode;
  not: boolean;
}

export const EMPTY_CONDITION: TextCondition = { term: '', mode: 'contains', not: false };

/**
 * Whether the condition narrows anything.
 *
 * ⚠️ The two modes trim differently, deliberately. A `contains` term of spaces is an accident and
 * matches everything anyway (`filterQuery.ts::textMatch` trims before comparing). A `regex` term of
 * spaces is a pattern — ` +` matches runs of spaces — and trimming it would silently change what
 * the operator asked for. Anything that decides "is this filtering?" must ask here rather than
 * testing `term !== ''`, or the two answers drift.
 */
export function conditionIsActive(c: TextCondition): boolean {
  return c.mode === 'regex' ? c.term !== '' : c.term.trim() !== '';
}

/** Characters that mean something at the start of an encoded value and so must be escaped there. */
const SIGNIFICANT = ['!', '~', '\\'];

/**
 * Encode to the single URL value: `[!][~]term`, with a leading `!`, `~` or `\` in the term escaped
 * by a backslash. An inactive condition encodes to `''` — the caller deletes the key.
 *
 * Mode and negation are **not** preserved when the term is empty, which is correct: no term means
 * no filter, and a URL that remembered `regex` for a search nobody is running would restore a
 * control into a state the view does not reflect. The editor keeps them in component state while
 * the operator is still typing; only the committed filter reaches the URL.
 */
export function encodeCondition(c: TextCondition): string {
  if (!conditionIsActive(c)) return '';
  const body = SIGNIFICANT.includes(c.term[0]) ? '\\' + c.term : c.term;
  return (c.not ? '!' : '') + (c.mode === 'regex' ? '~' : '') + body;
}

/** Decode a URL value. Anything malformed reads as a plain `contains` term rather than throwing —
 *  a hand-edited or stale URL must degrade to a wider view, never to a broken page. */
export function decodeCondition(value: string): TextCondition {
  let rest = value;
  const not = rest.startsWith('!');
  if (not) rest = rest.slice(1);
  const regex = rest.startsWith('~');
  if (regex) rest = rest.slice(1);
  if (rest.startsWith('\\')) rest = rest.slice(1);
  const cond: TextCondition = { term: rest, mode: regex ? 'regex' : 'contains', not };
  return conditionIsActive(cond) ? cond : EMPTY_CONDITION;
}

/**
 * Compile the condition into a matcher over a row's candidate strings, or `null` when it does not
 * filter.
 *
 * ⚠️ **An invalid regex matches nothing; it never throws.** The operator types the pattern one
 * character at a time, so `[` is a state every regex passes through. Throwing there takes the whole
 * table down mid-keystroke, and "no rows" is both survivable and visibly wrong in a way that tells
 * them to keep typing. The editor shows the compile error beside the box; this function stays
 * total.
 */
export function compileCondition(
  c: TextCondition,
): ((parts: readonly (string | null | undefined)[]) => boolean) | null {
  if (!conditionIsActive(c)) return null;
  if (c.mode === 'regex') {
    let re: RegExp;
    try {
      re = new RegExp(c.term, 'i');
    } catch {
      return () => c.not; // matches nothing, or everything when negated — the negation still holds
    }
    return (parts) => c.not !== parts.some((p) => p != null && re.test(p));
  }
  const needle = c.term.trim().toLowerCase();
  return (parts) => c.not !== parts.some((p) => p != null && p.toLowerCase().includes(needle));
}

/** The regex compile error to show beside the box, or `null` when there is nothing to say. */
export function conditionError(c: TextCondition): string | null {
  if (c.mode !== 'regex' || c.term === '') return null;
  try {
    new RegExp(c.term);
    return null;
  } catch (e) {
    return e instanceof Error ? e.message : String(e);
  }
}
