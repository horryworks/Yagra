// SPDX-License-Identifier: AGPL-3.0-only
// Where in a string the filter's term matched, so the UI can mark it (ADR-053 Inc.2e).
//
// 🚨 **This is the fourth implementation of "what does a plain term match".** The other three are
// PostgreSQL's `ILIKE`/`~*` predicate (`events.rs::EVENT_FILTER_WHERE`), the LogsQL builder
// (`logstore.rs::msg_prefix`), and the in-memory fake that stands in for the log store in tests
// (`logstore.rs::word_prefix_match`). The failure mode of a fourth copy is specific and nasty: this
// one is the only one the operator *looks at*, so when it drifts the screen lies — either marking
// text in a row the store did not match on that account, or showing an unmarked row and leaving
// them to wonder why it is in the list.
//
// The mitigation is a shared fixture rather than a shared implementation: `matchRanges.test.ts`
// asserts the same examples as the Rust fake's
// `the_fake_matches_from_a_word_start_like_the_real_index_does`, which were themselves transcribed
// from queries run against the real store. Change one, change both.
//
// The rule this file follows is **mark what the server matched on**, never "mark what the operator
// typed". Those differ in three places, and each is a case below: a negated condition (the rows on
// screen are the ones that did *not* match), a word-prefix deployment (`PERMIT` inside
// `POLICYPERMIT` is not why that row is here), and a widened query (where it is, because the second
// query looked inside words — ADR-053 Inc.2d).

import type { TextCondition } from './filterCondition';

/** A half-open `[start, end)` slice of the subject string. */
export type MatchRange = readonly [number, number];

/** How a plain term matches on this deployment. Mirrors `SearchSemantics` in `eventFilterSpec.ts`;
 *  `undefined` is an N-1 core that does not report it. */
export type MatchSemantics = 'prefix' | 'substring' | undefined;

/**
 * Every place `cond` matched inside `text`, in order, non-overlapping.
 *
 * `widened` says the screen re-asked this term as a substring because the plain query found nothing
 * (ADR-053 Inc.2d). It only means something when `semantics` is `'prefix'`.
 *
 * Returns `[]` rather than throwing for anything unmatchable — an empty term, a negation, a regex
 * that does not compile. The last is not an edge case: `[` is a state every pattern passes through
 * while it is being typed, and this runs on each keystroke.
 */
export function matchRanges(
  text: string,
  cond: TextCondition | null | undefined,
  semantics: MatchSemantics,
  widened = false,
): MatchRange[] {
  if (!cond || cond.not || cond.term === '' || text === '') return [];
  const re = buildRegex(cond, semantics, widened);
  if (!re) return [];

  const out: MatchRange[] = [];
  for (const m of text.matchAll(re)) {
    // A pattern able to match the empty string (`x*`) would otherwise spin here and produce one
    // zero-width range per character, none of which can be marked.
    if (m[0].length === 0) continue;
    out.push([m.index, m.index + m[0].length]);
    if (out.length >= MAX_RANGES) break;
  }
  return out;
}

/** Enough to mark what an operator can read in one cell or one popover. The cap exists because this
 *  runs per visible row on every render, and a term like `=` occurs dozens of times in one syslog
 *  line — past a point the marks stop being information and start being a highlighter accident. */
const MAX_RANGES = 200;

function buildRegex(
  cond: TextCondition,
  semantics: MatchSemantics,
  widened: boolean,
): RegExp | null {
  try {
    if (cond.mode === 'regex') return new RegExp(cond.term, 'gi');
    const body = escapeForRegex(cond.term);
    // A word prefix, and only when the store actually answered it that way. The boundary is spelled
    // out rather than left to `\b` because `\b` is defined relative to the *term's* first character,
    // and a term may start with a separator (`=public`); this asks the question the store asks —
    // "does a word start here" — whatever the term looks like.
    //
    // Measured against the live store (2026-08-13) rather than assumed, because the obvious guess
    // was wrong: `-` `.` `=` `/` `%` all separate words, but **`_` does not**. `i("to"*)` returns
    // 836 rows while `Trust_to_Untrust` appears in 111,021 of them, so the underscore is inside the
    // word. A word is therefore `[a-z0-9_]`, which is also what `\w` means.
    if (semantics === 'prefix' && !widened) return new RegExp(`(?<![a-z0-9_])${body}`, 'gi');
    return new RegExp(body, 'gi');
  } catch {
    return null;
  }
}

/** Duplicated deliberately from `eventFilterSpec.ts::escapeRegex` — see that function's note about
 *  the backslash. Importing it here would make a `lib/` module depend on a screen's module, which is
 *  backwards; the two are three lines and are covered by tests on both sides. */
function escapeForRegex(term: string): string {
  return term.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/** The string split into alternating plain and marked pieces, ready to render.
 *
 * Returned as data rather than as elements so the judgement stays in a `.ts` where tests run, and
 * so no caller is ever tempted to build markup out of device-supplied text — the message is
 * attacker-influenced and is rendered as text only (never `dangerouslySetInnerHTML`).
 */
export function markedSegments(
  text: string,
  ranges: readonly MatchRange[],
): { text: string; marked: boolean }[] {
  if (ranges.length === 0) return [{ text, marked: false }];
  const out: { text: string; marked: boolean }[] = [];
  let at = 0;
  for (const [start, end] of ranges) {
    if (start > at) out.push({ text: text.slice(at, start), marked: false });
    out.push({ text: text.slice(start, end), marked: true });
    at = end;
  }
  if (at < text.length) out.push({ text: text.slice(at), marked: false });
  return out;
}
