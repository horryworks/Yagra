// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  JOINS,
  appliesToBody,
  appliesToFrom,
  bracketsTags,
  folderChoices,
  joinMatters,
  namesNothing,
  reachDetail,
  shownNames,
  spaceBefore,
  tagMatchMatters,
  tagSideText,
} from './appliesToTarget';
import { TAG_MATCH_LABEL, TAG_WORD } from '../../lib/tagMatch';
import { TAG_MATCHES } from '../../types/api';
import en from '../../locales/en/common.json';
import ja from '../../locales/ja/common.json';

describe('appliesTo', () => {
  it('reads a stored pair with nothing in it as every node, and anything in it as only these', () => {
    expect(appliesToFrom([], []).only).toBe(false);
    expect(appliesToFrom(['g'], []).only).toBe(true);
    expect(appliesToFrom([], ['core']).only).toBe(true);
  });

  it('sends nothing while every node is chosen, keeping the ticks for a switch back', () => {
    const v = { only: false, groupIds: ['g'], tags: ['core'], join: 'and' as const, tagMatch: 'any' as const };
    expect(appliesToBody(v)).toEqual({ group_ids: [], tags: [], join: 'or', tag_match: 'any' });
    expect(appliesToBody({ ...v, only: true, tagMatch: 'all' })).toEqual({
      group_ids: ['g'],
      tags: ['core'],
      join: 'and',
      tag_match: 'all',
    });
  });

  it('calls "only these" with nothing ticked a choice of nothing', () => {
    expect(namesNothing({ only: true, groupIds: [], tags: [], join: 'or', tagMatch: 'any' as const })).toBe(true);
    expect(namesNothing({ only: false, groupIds: [], tags: [], join: 'or', tagMatch: 'any' as const })).toBe(false);
    expect(namesNothing({ only: true, groupIds: [], tags: ['x'], join: 'or', tagMatch: 'any' as const })).toBe(false);
  });

  it('reads a stored setting with no join as or, the way every rule before the switch reads', () => {
    expect(appliesToFrom(['g'], ['core']).join).toBe('or');
    expect(appliesToFrom(['g'], ['core'], 'and').join).toBe('and');
    expect(JOINS).toEqual(['or', 'and']);
  });

  it('offers or / and only when both lists name something', () => {
    const v = appliesToFrom(['g'], ['core']);
    expect(joinMatters(v)).toBe(true);
    expect(joinMatters({ ...v, tags: [] })).toBe(false);
    expect(joinMatters({ ...v, groupIds: [] })).toBe(false);
    expect(joinMatters({ ...v, only: false })).toBe(false);
  });

  it('shows the first two names and counts the rest', () => {
    expect(shownNames(['a'])).toEqual({ shown: ['a'], more: 0 });
    expect(shownNames(['a', 'b', 'c', 'd'])).toEqual({ shown: ['a', 'b'], more: 2 });
  });

  it('explains the reach by both counts under or, by the folder count under and', () => {
    const parts = { by_folder: 6, by_tag: 7, both: 5 };
    expect(reachDetail(parts, 'or')).toEqual({
      key: 'appliesTo.partsOr',
      values: { folders: 6, tags: 7, both: 5 },
    });
    expect(reachDetail(parts, 'and')).toEqual({
      key: 'appliesTo.partsAnd',
      values: { folders: 6 },
    });
  });

  it('lists a named folder that no longer exists first, then every folder by path', () => {
    const options = [
      { id: 'a', label: 'Tokyo', depth: 0, path: 'Tokyo' },
      { id: 'b', label: 'Edge', depth: 1, path: 'Tokyo / Edge' },
    ];
    expect(folderChoices(options, ['b', 'gone'])).toEqual([
      { id: 'gone', label: 'gone', gone: true },
      { id: 'a', label: 'Tokyo', gone: false },
      { id: 'b', label: 'Tokyo / Edge', gone: false },
    ]);
  });
});

describe('has any / has all (ADR-267)', () => {
  it('reads a stored setting with no tag_match as any, the way every setting before it reads', () => {
    expect(appliesToFrom([], ['core', '24x7']).tagMatch).toBe('any');
    expect(appliesToFrom([], ['core', '24x7'], 'or', 'all').tagMatch).toBe('all');
  });

  it('offers the switch only for two or more tags, and only while "only these" is chosen', () => {
    expect(tagMatchMatters({ only: true, tags: ['core'] })).toBe(false);
    expect(tagMatchMatters({ only: true, tags: ['core', '24x7'] })).toBe(true);
    expect(tagMatchMatters({ only: false, tags: ['core', '24x7'] })).toBe(false);
  });

  it('brackets the tags only when folders stand beside two or more of them', () => {
    expect(bracketsTags(1, 2)).toBe(true);
    expect(bracketsTags(0, 2)).toBe(false);
    expect(bracketsTags(1, 1)).toBe(false);
  });

  it('writes the tag side with the word, the count and the brackets it is given', () => {
    const o = { open: '(', close: ')', more: (n: number) => `+${n} tags` };
    expect(tagSideText(['core', '24x7'], { ...o, word: 'and', bracket: true })).toBe('(core and 24x7)');
    expect(tagSideText(['core', '24x7', 'edge'], { ...o, word: 'or', bracket: false })).toBe(
      'core or 24x7 +1 tags',
    );
    expect(tagSideText(['core'], { ...o, word: 'and', bracket: false })).toBe('core');
  });

  it('puts no half-width space before a full-width bracket, one before anything else', () => {
    expect(spaceBefore('（core かつ 24x7）')).toBe('');
    expect(spaceBefore('(core and 24x7)')).toBe(' ');
    expect(spaceBefore('core')).toBe(' ');
    // In Japanese the joining word meets the full-width bracket with no space between them.
    expect(`東京 または${spaceBefore(ja.tagMatch.open)}${ja.tagMatch.open}`).toBe('東京 または（');
  });

  it('has a label and a word for both sides, in both languages', () => {
    for (const m of TAG_MATCHES) {
      for (const lang of [en, ja]) {
        const pick = (key: string) => lang.tagMatch[key.split('.').pop() as keyof typeof lang.tagMatch];
        expect(pick(TAG_MATCH_LABEL[m])).toBeTruthy();
        expect(pick(TAG_WORD[m])).toBeTruthy();
      }
    }
    expect([en.tagMatch.or, en.tagMatch.and]).toEqual(['or', 'and']);
    expect([ja.tagMatch.or, ja.tagMatch.and]).toEqual(['または', 'かつ']);
  });
});
