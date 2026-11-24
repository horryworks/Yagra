// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { canonicalLabel, enterValue, moveActive, shownSuggestions } from './chipSuggest';

const TAGS = ['MATSUYAMA', 'OSAKA', 'core'];

describe('shownSuggestions', () => {
  it('lists every suggestion not yet chosen when nothing is typed', () => {
    expect(shownSuggestions(TAGS, [], '')).toEqual(TAGS);
    expect(shownSuggestions(TAGS, ['OSAKA'], '  ')).toEqual(['MATSUYAMA', 'core']);
  });

  it('narrows to the ones containing the typed text, ignoring case', () => {
    expect(shownSuggestions(TAGS, [], 'mat')).toEqual(['MATSUYAMA']);
    expect(shownSuggestions(TAGS, [], 'YAM')).toEqual(['MATSUYAMA']);
    expect(shownSuggestions(TAGS, [], 'o')).toEqual(['OSAKA', 'core']);
    expect(shownSuggestions(TAGS, [], 'tokyo')).toEqual([]);
  });

  it('never offers a value already chosen', () => {
    expect(shownSuggestions(TAGS, ['MATSUYAMA'], 'mat')).toEqual([]);
  });
});

describe('moveActive', () => {
  it('walks down and stops at the last row', () => {
    expect(moveActive(-1, 3, 1)).toBe(0);
    expect(moveActive(2, 3, 1)).toBe(2);
  });

  it('walks up back to no row', () => {
    expect(moveActive(0, 3, -1)).toBe(-1);
    expect(moveActive(-1, 3, -1)).toBe(-1);
  });

  it('has no row to move to in an empty list', () => {
    expect(moveActive(-1, 0, 1)).toBe(-1);
  });
});

describe('enterValue', () => {
  it('adds the highlighted suggestion', () => {
    expect(enterValue(['MATSUYAMA', 'OSAKA'], 1, 'a')).toBe('OSAKA');
  });

  it('adds the typed text when nothing is highlighted', () => {
    expect(enterValue(['MATSUYAMA'], -1, 'mats')).toBe('mats');
    expect(enterValue([], -1, 'new-tag')).toBe('new-tag');
  });

  it('takes the suggestion spelling for a case-only difference', () => {
    expect(enterValue(['MATSUYAMA'], -1, 'matsuyama')).toBe('MATSUYAMA');
  });

  it('adds nothing for an empty box', () => {
    expect(enterValue(['MATSUYAMA'], -1, '  ')).toBeNull();
  });
});

describe('canonicalLabel', () => {
  // Blur and paste commit through this too, not only Enter on an open list: a case-only variant
  // of a known tag must not become a second tag.
  it('takes the known spelling for a case-only difference', () => {
    expect(canonicalLabel('matsuyama', ['MATSUYAMA', 'core'])).toBe('MATSUYAMA');
    expect(canonicalLabel(' Core ', ['MATSUYAMA', 'core'])).toBe('core');
  });

  it('keeps a label that names nothing known', () => {
    expect(canonicalLabel('tokyo', ['MATSUYAMA'])).toBe('tokyo');
    expect(canonicalLabel('', ['MATSUYAMA'])).toBe('');
  });
});
