// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { flattenValues, tokenDiff, tokens } from './i18nTokens';

describe('tokens', () => {
  it('counts placeholders and tags, and folds a closing tag into its opening one', () => {
    // Placeholders are collected before tags, so the map's order is theirs first.
    expect([...tokens('Remove {{name}} from <b>{{group}}</b>')]).toEqual([
      ['{{name}}', 1],
      ['{{group}}', 1],
      ['<b>', 2],
    ]);
  });

  it('is empty for plain text', () => {
    expect(tokens('No rows.').size).toBe(0);
  });
});

describe('tokenDiff', () => {
  it('is silent when the multisets agree, whatever the order', () => {
    expect(tokenDiff('{{count}} of {{total}}', '{{total}} 件中 {{count}} 件')).toBe('');
    expect(tokenDiff('Open <lnk>the log</lnk>', '<lnk>ログ</lnk>を開く')).toBe('');
  });

  it('names a placeholder the translation dropped', () => {
    // The failure this guards: the number silently gone from a Japanese sentence.
    expect(tokenDiff('{{count}} nodes', 'ノード')).toBe('{{count}} ×1 in en, ×0 in ja');
  });

  it('names a tag the translation invented', () => {
    expect(tokenDiff('Sign in', '<b>サインイン</b>')).toBe('<b> ×0 in en, ×2 in ja');
  });
});

describe('flattenValues', () => {
  it('keeps only string leaves, keyed by dot path', () => {
    expect([...flattenValues({ a: { b: 'x', c: 3 }, d: 'y', e: ['z'] })]).toEqual([
      ['a.b', 'x'],
      ['d', 'y'],
    ]);
  });
});
