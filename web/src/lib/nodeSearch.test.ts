// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import cases from './fixtures/searchWords.cases.json';
import { everyWordIn, looksLikeAddressPart, searchWords } from './nodeSearch';

// The table `yagra_common::node_search`'s tests run too. A new case goes in the JSON, never here.
describe('the search words answer the case table core shares', () => {
  it('splits a term into words', () => {
    expect(cases.words.length).toBeGreaterThanOrEqual(10);
    for (const c of cases.words) expect(searchWords(c.term), JSON.stringify(c.term)).toEqual(c.words);
  });

  it('shows a folder whole only when every word is on its path', () => {
    expect(cases.paths.length).toBeGreaterThanOrEqual(6);
    for (const c of cases.paths) {
      const words = searchWords(c.term) ?? [];
      expect(everyWordIn(words, c.path), `${c.term} over ${c.path.join('/')}`).toBe(c.whole);
    }
  });

  it('knows which words could be part of an address', () => {
    expect(cases.address.length).toBeGreaterThanOrEqual(6);
    for (const c of cases.address) expect(looksLikeAddressPart(c.word), c.word).toBe(c.address);
  });
});
