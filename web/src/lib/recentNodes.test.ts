// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { RECENT_NODES_MAX, parseRecent, pushRecent } from './recentNodes';

describe('recent nodes', () => {
  it('puts the newest first and does not repeat one', () => {
    expect(pushRecent(['a', 'b', 'c'], 'b')).toEqual(['b', 'a', 'c']);
  });

  it('keeps at most the limit', () => {
    const many = Array.from({ length: RECENT_NODES_MAX }, (_, i) => `n${i}`);
    const next = pushRecent(many, 'new');
    expect(next).toHaveLength(RECENT_NODES_MAX);
    expect(next[0]).toBe('new');
    expect(next).not.toContain(`n${RECENT_NODES_MAX - 1}`);
  });

  it('reads anything but a list of strings as nothing remembered', () => {
    expect(parseRecent(null)).toEqual([]);
    expect(parseRecent('not json')).toEqual([]);
    expect(parseRecent('{"a":1}')).toEqual([]);
    expect(parseRecent('["a",2,"b"]')).toEqual(['a', 'b']);
  });
});
