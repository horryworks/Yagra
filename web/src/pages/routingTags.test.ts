// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { tagChoices, tagsNoNodeCarries } from './routingTags';

const places = { own: 0, folders: 0, refusals: 0, rules: 0, exceptions: 0 };
const carried = {
  tags: [
    { tag: 'APAC', nodes: 388, ...places },
    { tag: 'TeamA', nodes: 214, ...places },
    // Named only by a rule (ADR-263): listed by the server, offered by no picker.
    { tag: 'KOREA', nodes: 0, ...places, rules: 1 },
  ],
};

describe('the routing rule tag list', () => {
  it('lists a rule tag no node carries first, then every carried tag with its count', () => {
    expect(tagChoices(['TeamA', 'KOREA'], carried)).toEqual([
      { tag: 'KOREA', nodes: 0, gone: true },
      { tag: 'APAC', nodes: 388, gone: false },
      { tag: 'TeamA', nodes: 214, gone: false },
    ]);
  });

  it('keeps the rule tags, unmarked, when the carried tags are unknown', () => {
    expect(tagChoices(['KOREA'], null)).toEqual([{ tag: 'KOREA', nodes: null, gone: false }]);
  });

  it('marks the rule tags no node carries, and nothing when the list is unknown', () => {
    expect([...(tagsNoNodeCarries(['TeamA', 'KOREA'], carried) ?? [])]).toEqual(['KOREA']);
    expect(tagsNoNodeCarries(['KOREA'], null)).toBeNull();
  });
});
