// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for match highlighting (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { markedSegments, matchRanges, type MatchSemantics } from './matchRanges';
import type { TextCondition } from './filterCondition';

const c = (term: string, mode: TextCondition['mode'] = 'contains', not = false): TextCondition => ({
  term,
  mode,
  not,
});

/** The real capture this whole feature was reported against. */
const MSG = 'Aug 13 2026 13:05:51 jpmyj01fw01 %%01POLICY/6/POLICYPERMIT(l):CID=0x814f041e;vsys=public';

const marked = (text: string, ...args: Parameters<typeof matchRanges> extends [string, ...infer R] ? R : never) =>
  matchRanges(text, ...args).map(([s, e]) => text.slice(s, e));

describe('matchRanges — a plain term on a word-prefix deployment', () => {
  // ⚠️ THE SAME CASES AS `logstore.rs::the_fake_matches_from_a_word_start_like_the_real_index_does`,
  // which were themselves taken from queries run against the live store. This is the fourth
  // implementation of "what does a plain term match" (PostgreSQL, LogsQL, the Rust fake, here), and
  // a shared fixture is the only thing keeping them honest. If you change one, change both — the
  // symptom of drift is a screen that marks text the store never matched on, or shows an unmarked
  // row and leaves the operator wondering why it is in the list.
  const cases: [string, boolean, string][] = [
    ['policy', true, 'a word starts with it — the case this feature is for'],
    ['policypermit', true, 'an exact word is a prefix of itself'],
    ['ermit', false, 'inside a word: `i("ermit"*)` returned 0 on the real store'],
    ['permit', false, "the operator's complaint — only the widened query finds this"],
    ['cid=0x814f', true, 'separators are literal; only the start must land on a word'],
    ['policy/6', false, 'the word is `01POLICY`, so the phrase never starts'],
    ['POLICY', true, 'case-insensitive, which is what `i(…)` buys'],
  ];

  for (const [term, want, why] of cases) {
    it(`${want ? 'marks' : 'does not mark'} ${JSON.stringify(term)} — ${why}`, () => {
      expect(matchRanges(MSG, c(term), 'prefix').length > 0).toBe(want);
    });
  }

  it('marks every word that starts with the term, not just the first', () => {
    expect(marked('policy POLICYPERMIT nopolicy', c('policy'), 'prefix')).toEqual([
      'policy',
      'POLICYPERMIT'.slice(0, 6),
    ]);
  });

  it('treats an underscore as part of a word and every other separator as a break', () => {
    // ⚠️ Measured, not guessed — the guess was wrong. On the live store `i("to"*)` returns 836 rows
    // while `Trust_to_Untrust` occurs in 111,021 of them, so `_` is *inside* the word. `-` `.` `=`
    // `/` all break it (`i("zone"*)` finds `source-zone=trust`, `i("168"*)` finds `192.168.1.119`).
    expect(matchRanges('x_policy', c('policy'), 'prefix')).toEqual([]);
    for (const sep of ['-', '.', '=', '/', ' ', ':']) {
      expect(matchRanges(`x${sep}policy`, c('policy'), 'prefix')).toHaveLength(1);
    }
  });
});

describe('matchRanges — the other three ways a term is matched', () => {
  it('marks any substring on a PostgreSQL deployment', () => {
    // The two backends are permitted to differ here, so the marks differ with them.
    expect(marked(MSG, c('permit'), 'substring')).toEqual(['PERMIT']);
  });

  it('marks any substring once the query has been widened', () => {
    // ADR-053 Inc.2d: the plain query found nothing and was re-asked as a substring, so `PERMIT`
    // inside `POLICYPERMIT` really is why this row is on screen.
    expect(matchRanges(MSG, c('permit'), 'prefix')).toEqual([]);
    expect(marked(MSG, c('permit'), 'prefix', true)).toEqual(['PERMIT']);
  });

  it('marks what the pattern matched in regex mode', () => {
    expect(marked(MSG, c('POLICY.*PERMIT', 'regex'), 'prefix')).toEqual([
      'POLICY/6/POLICYPERMIT',
    ]);
  });

  it('falls back to substring when the core did not say how it matches', () => {
    // An N-1 core sends no `search_semantics`. It also ignores the filter entirely, so the rows are
    // unfiltered — marking the term where it appears is the most useful thing left to do, and the
    // widest reading is the honest one when the narrow one cannot be justified.
    expect(marked(MSG, c('permit'), undefined)).toEqual(['PERMIT']);
  });
});

describe('matchRanges — the cases that must produce nothing rather than throw', () => {
  it('marks nothing for a negated condition', () => {
    // ⚠️ The rows on screen are the ones that did NOT match, so any mark would point at the reason a
    // row should have been excluded — the opposite of what a highlight means.
    for (const s of ['prefix', 'substring'] as MatchSemantics[]) {
      expect(matchRanges(MSG, c('policy', 'contains', true), s)).toEqual([]);
      expect(matchRanges(MSG, c('POLICY.*', 'regex', true), s)).toEqual([]);
    }
  });

  it('marks nothing for an empty term, an empty subject, or no condition at all', () => {
    expect(matchRanges(MSG, c(''), 'prefix')).toEqual([]);
    expect(matchRanges('', c('policy'), 'prefix')).toEqual([]);
    expect(matchRanges(MSG, null, 'prefix')).toEqual([]);
    expect(matchRanges(MSG, undefined, 'prefix')).toEqual([]);
  });

  it('marks nothing for a pattern that does not compile, and does not throw', () => {
    // `[` is a state every regex passes through while it is being typed, and this runs per row per
    // keystroke. Throwing here would blank the table mid-word.
    for (const bad of ['[', '(', '\\', '(?<']) {
      expect(() => matchRanges(MSG, c(bad, 'regex'), 'prefix')).not.toThrow();
      expect(matchRanges(MSG, c(bad, 'regex'), 'prefix')).toEqual([]);
    }
  });

  it('terminates on a pattern that can match nothing at all', () => {
    // A zero-width match advances `matchAll` by one character forever; every one of them would be an
    // unmarkable range. There is nothing to show, and the loop has to end.
    expect(matchRanges(MSG, c('z*', 'regex'), 'prefix')).toEqual([]); // no `z` in the subject
    expect(matchRanges(MSG, c('(?:)', 'regex'), 'prefix')).toEqual([]);
  });

  it('escapes the term in plain mode, so a metacharacter marks itself', () => {
    expect(marked('1.2.3.4 and 1x2y3z4', c('1.2.3.4'), 'substring')).toEqual(['1.2.3.4']);
    expect(marked('a+b', c('a+b'), 'substring')).toEqual(['a+b']);
    expect(marked('C:\\Users', c('C:\\Users'), 'substring')).toEqual(['C:\\Users']);
  });

  it('caps how many marks one string can carry', () => {
    const many = '='.repeat(500);
    expect(matchRanges(many, c('='), 'substring')).toHaveLength(200);
  });
});

describe('markedSegments', () => {
  it('returns the whole string as one plain piece when nothing matched', () => {
    expect(markedSegments('abc', [])).toEqual([{ text: 'abc', marked: false }]);
  });

  it('splits into alternating pieces that rejoin to the original', () => {
    const text = 'a policy b policy';
    const segs = markedSegments(text, matchRanges(text, c('policy'), 'prefix'));
    expect(segs).toEqual([
      { text: 'a ', marked: false },
      { text: 'policy', marked: true },
      { text: ' b ', marked: false },
      { text: 'policy', marked: true },
    ]);
    // The property that matters: rendering the pieces shows the message, unaltered.
    expect(segs.map((s) => s.text).join('')).toBe(text);
  });

  it('handles a match at each end without emitting empty pieces', () => {
    expect(markedSegments('ab', [[0, 1]])).toEqual([
      { text: 'a', marked: true },
      { text: 'b', marked: false },
    ]);
    expect(markedSegments('ab', [[1, 2]])).toEqual([
      { text: 'a', marked: false },
      { text: 'b', marked: true },
    ]);
    expect(markedSegments('ab', [[0, 2]])).toEqual([{ text: 'ab', marked: true }]);
  });

  it('rejoins to the original for every case in this file', () => {
    for (const term of ['policy', 'permit', 'cid=0x814f', '=']) {
      for (const s of ['prefix', 'substring'] as MatchSemantics[]) {
        const segs = markedSegments(MSG, matchRanges(MSG, c(term), s));
        expect(segs.map((x) => x.text).join('')).toBe(MSG);
      }
    }
  });
});
