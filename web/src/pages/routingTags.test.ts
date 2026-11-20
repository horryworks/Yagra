// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { tagOptions, tagsNoNodeCarries } from './routingTags';

const carried = {
  tags: [
    { tag: 'APAC', nodes: 388 },
    { tag: 'TeamA', nodes: 214 },
  ],
};

describe('the routing rule tag picker', () => {
  it('offers every carried tag with its node count', () => {
    expect(tagOptions(carried, (n) => `${n} nodes`)).toEqual([
      { value: 'APAC', detail: '388 nodes' },
      { value: 'TeamA', detail: '214 nodes' },
    ]);
  });

  it('marks the rule tags no node carries, and nothing when the list is unknown', () => {
    expect([...(tagsNoNodeCarries(['TeamA', 'KOREA'], carried) ?? [])]).toEqual(['KOREA']);
    expect(tagsNoNodeCarries(['KOREA'], null)).toBeNull();
  });
});
