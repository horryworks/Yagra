// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  TAG_IDLE_LIMIT,
  TAG_TYPED_LIMIT,
  tagPickerView,
  tagRequestLimit,
  toggleTag,
} from './tagPicker';

const answer = (tags: [string, number][], more = 0, chosen: [string, number][] = []) => ({
  tags: tags.map(([tag, nodes]) => ({ tag, nodes })),
  more,
  chosen: chosen.map(([tag, nodes]) => ({ tag, nodes })),
});

describe('tagPicker', () => {
  it('asks for the offer plus the chosen tags, capped at the server maximum', () => {
    expect(tagRequestLimit('', 0)).toBe(TAG_IDLE_LIMIT);
    expect(tagRequestLimit(' co ', 2)).toBe(TAG_TYPED_LIMIT + 2);
    expect(tagRequestLimit('co', 40)).toBe(50);
  });

  it('puts the chosen tags first and does not offer them twice', () => {
    const v = tagPickerView(answer([['wifi', 9], ['core', 5], ['wan', 3]]), 'wan', '');
    expect(v.options).toEqual(['wan', 'wifi', 'core']);
    expect(v.counts.wan).toBe(3);
  });

  it('keeps a chosen tag that does not match the text, with its own count', () => {
    const v = tagPickerView(answer([['wifi', 9]], 0, [['old-tag', 4]]), 'old-tag', 'wi');
    expect(v.options).toEqual(['old-tag', 'wifi']);
    expect(v.counts['old-tag']).toBe(4);
  });

  it('offers at most eight with nothing typed and counts the rest as more', () => {
    const many = Array.from({ length: 10 }, (_, i): [string, number] => [`t${i}`, 10 - i]);
    const v = tagPickerView(answer(many, 4312), '', '');
    expect(v.options).toHaveLength(TAG_IDLE_LIMIT);
    expect(v.more).toBe(4312 + 2);
  });

  it('draws nothing but the chosen tags before the first answer', () => {
    expect(tagPickerView(null, 'a,b', '')).toEqual({ options: ['a', 'b'], counts: {}, more: 0 });
  });

  it('toggles one tag and keeps the picking order', () => {
    expect(toggleTag('', 'core')).toBe('core');
    expect(toggleTag('core', 'wan')).toBe('core,wan');
    expect(toggleTag('core,wan', 'core')).toBe('wan');
  });
});
