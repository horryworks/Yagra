// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the text-condition codec (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { TEXT_MODES, type TextMode } from './columnFilter';
import {
  compileCondition,
  conditionEcho,
  conditionError,
  conditionIsActive,
  decodeCondition,
  encodeCondition,
  EMPTY_CONDITION,
  type TextCondition,
} from './filterCondition';

const c = (term: string, mode: TextMode = 'contains', not = false): TextCondition => ({
  term,
  mode,
  not,
});

describe('the codec round-trips', () => {
  // The whole reason three dimensions share one URL key is that "at its default ⇒ delete the key"
  // must be one delete. The price is this escape rule, so it gets the exhaustive test.
  const terms = [
    'link',
    'link down',
    '!link', // the negation marker, as data
    '~link', // the regex marker, as data
    '\\link', // the escape itself, as data
    '\\!link',
    '!!link',
    '~~link',
    'a!b~c\\d', // significant characters anywhere but the front are not special
    '^%LINK-3-UPDOWN',
    '  padded  ',
    'ü日本語',
    '100%',
    'a,b', // comma is the SET separator, not this codec's — it must survive here
  ];

  for (const term of terms) {
    for (const mode of TEXT_MODES) {
      for (const not of [false, true]) {
        it(`survives ${JSON.stringify(term)} / ${mode} / not=${not}`, () => {
          const original = c(term, mode, not);
          expect(decodeCondition(encodeCondition(original))).toEqual(original);
        });
      }
    }
  }

  it('encodes an inactive condition to the empty value, so the caller deletes the key', () => {
    expect(encodeCondition(c(''))).toBe('');
    expect(encodeCondition(c('', 'regex', true))).toBe('');
    // A `contains` term of only spaces matches everything, so it is not a filter.
    expect(encodeCondition(c('   '))).toBe('');
    expect(decodeCondition('')).toEqual(EMPTY_CONDITION);
  });

  it('keeps a whitespace-only REGEX term, because there it is a pattern', () => {
    // ` +` matches runs of spaces; trimming would change what the operator asked for. This is the
    // one place the two modes must not share a rule.
    expect(conditionIsActive(c(' ', 'regex'))).toBe(true);
    expect(conditionIsActive(c(' ', 'contains'))).toBe(false);
    expect(decodeCondition(encodeCondition(c(' ', 'regex')))).toEqual(c(' ', 'regex'));
  });

  it('forgets mode and negation when there is no term', () => {
    // A URL that remembered `regex` for a search nobody is running would restore a control into a
    // state the view does not reflect.
    expect(decodeCondition(encodeCondition(c('', 'regex', true)))).toEqual(EMPTY_CONDITION);
  });

  it('reads a malformed value as a plain term instead of throwing', () => {
    // A hand-edited or stale URL must degrade to a wider view, never to a broken page.
    expect(decodeCondition('~')).toEqual(EMPTY_CONDITION);
    expect(decodeCondition('!')).toEqual(EMPTY_CONDITION);
    expect(decodeCondition('!~')).toEqual(EMPTY_CONDITION);
    expect(decodeCondition('\\')).toEqual(EMPTY_CONDITION);
    expect(decodeCondition('!~\\')).toEqual(EMPTY_CONDITION);
  });

  it('spells the wire form the way the URL shows it', () => {
    // Pinned verbatim: this is what lands in a bookmark, so a change here breaks shared links.
    expect(encodeCondition(c('link'))).toBe('link');
    expect(encodeCondition(c('link', 'contains', true))).toBe('!link');
    expect(encodeCondition(c('^LINK', 'regex'))).toBe('~^LINK');
    expect(encodeCondition(c('^LINK', 'regex', true))).toBe('!~^LINK');
  });
});

describe('compileCondition', () => {
  it('returns null when the condition does not narrow', () => {
    expect(compileCondition(c(''))).toBeNull();
    expect(compileCondition(c('  '))).toBeNull();
  });

  it('matches a substring, case-insensitively', () => {
    const m = compileCondition(c('LINK'))!;
    expect(m(['%link-3-updown'])).toBe(true);
    expect(m(['nothing here'])).toBe(false);
  });

  it('matches inside a token, which the log store deliberately does not', () => {
    // `POLICY` finds `POLICYPERMIT` in the browser. On VictoriaLogs it does not — a plain term is
    // whole-token there, and that divergence is measured, intentional and guarded
    // (`a_plain_term_stays_a_phrase_filter_not_a_regex_scan`). Pinning the client side here is what
    // makes "the two behave differently" a statement someone can check rather than a surprise.
    const m = compileCondition(c('POLICY'))!;
    expect(m(['%%01POLICY/6/POLICYPERMIT(l):CID=0x814f'])).toBe(true);
  });

  it('ignores null and undefined parts rather than crashing on them', () => {
    const m = compileCondition(c('link'))!;
    expect(m([null, undefined, 'link down'])).toBe(true);
    expect(m([null, undefined])).toBe(false);
  });

  it('inverts on NOT, so yes and no partition the rows', () => {
    const rows = [['link down'], ['bgp up'], ['link up'], [null]];
    const yes = compileCondition(c('link'))!;
    const no = compileCondition(c('link', 'contains', true))!;
    expect(rows.filter(yes).length + rows.filter(no).length).toBe(rows.length);
    expect(rows.filter(yes).length).toBe(2);
  });

  it('runs a regex case-insensitively and anchored where asked', () => {
    const m = compileCondition(c('^%LINK-3', 'regex'))!;
    expect(m(['%link-3-updown'])).toBe(true);
    expect(m(['prefix %LINK-3-UPDOWN'])).toBe(false);
  });

  it('matches nothing on an invalid regex, and never throws', () => {
    // `[` is a state every regex passes through while it is being typed. Throwing there takes the
    // table down mid-keystroke.
    const m = compileCondition(c('[', 'regex'))!;
    expect(() => m(['anything'])).not.toThrow();
    expect(m(['anything'])).toBe(false);
  });

  it('matches everything on an invalid NEGATED regex', () => {
    // Nothing matches the broken pattern, so nothing is excluded. The negation still holds.
    const m = compileCondition(c('(', 'regex', true))!;
    expect(m(['anything'])).toBe(true);
  });
});

describe('conditionError', () => {
  it('is silent unless a regex is actually broken', () => {
    expect(conditionError(c('['))).toBeNull(); // contains mode: `[` is just a character
    expect(conditionError(c('', 'regex'))).toBeNull();
    expect(conditionError(c('^ok$', 'regex'))).toBeNull();
    expect(conditionError(c('[', 'regex'))).toBeTruthy();
  });
});

describe('conditionEcho — what the editor will read back', () => {
  it('reports the default for an inactive condition, whatever mode it carried', () => {
    // ⚠️ The fact the editor got wrong. Turning on Regex with an empty box produces a condition
    // that encodes to '' and therefore comes back as the default — so an editor comparing against
    // what it *sent* sees a difference, calls it an outside edit, and resets the switch. The
    // symptom is a switch that cannot be moved until something is typed.
    for (const mode of TEXT_MODES) {
      for (const not of [false, true]) {
        expect(conditionEcho(c('', mode, not))).toEqual(EMPTY_CONDITION);
      }
    }
    // Whitespace is inactive in `contains` and meaningful in `regex` — the echo has to follow
    // `conditionIsActive`, not `term !== ''`, or the two answers drift.
    expect(conditionEcho(c('   ', 'contains'))).toEqual(EMPTY_CONDITION);
    expect(conditionEcho(c('   ', 'regex'))).toEqual(c('   ', 'regex'));
  });

  it('is the identity on an active condition, including the escaped terms', () => {
    for (const cond of [
      c('link down'),
      c('!link'),
      c('~link'),
      c('\\link'),
      c('^%LINK-3', 'regex'),
      c('policypermit', 'contains', true),
      c('^%LINK-3', 'regex', true),
    ]) {
      expect(conditionEcho(cond)).toEqual(cond);
    }
  });
});
