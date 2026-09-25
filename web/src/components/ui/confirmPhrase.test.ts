// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { confirmPhraseMatches } from './confirmPhrase';

describe('confirmPhraseMatches', () => {
  it('matches the exact name, ignoring surrounding whitespace', () => {
    expect(confirmPhraseMatches('Tokyo DC', 'Tokyo DC')).toBe(true);
    expect(confirmPhraseMatches('  Tokyo DC ', 'Tokyo DC')).toBe(true);
  });

  it('refuses a near miss', () => {
    expect(confirmPhraseMatches('tokyo dc', 'Tokyo DC')).toBe(false);
    expect(confirmPhraseMatches('Tokyo', 'Tokyo DC')).toBe(false);
    expect(confirmPhraseMatches('TokyoDC', 'Tokyo DC')).toBe(false);
    expect(confirmPhraseMatches('', 'Tokyo DC')).toBe(false);
  });

  it('never confirms against an empty phrase', () => {
    expect(confirmPhraseMatches('', '')).toBe(false);
    expect(confirmPhraseMatches('  ', ' ')).toBe(false);
  });
});
