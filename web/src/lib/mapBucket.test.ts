// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { pushInto } from './mapBucket';

describe('pushInto', () => {
  it('creates the bucket on the first append', () => {
    const m = new Map<string, number[]>();
    pushInto(m, 'a', 1);
    expect(m.get('a')).toEqual([1]);
  });

  it('appends in insertion order', () => {
    // Load-bearing: every caller either sorts afterwards or keeps the API's order deliberately.
    const m = new Map<string, number[]>();
    pushInto(m, 'a', 1);
    pushInto(m, 'a', 2);
    pushInto(m, 'a', 3);
    expect(m.get('a')).toEqual([1, 2, 3]);
  });

  it('keeps buckets separate', () => {
    const m = new Map<string, number[]>();
    pushInto(m, 'a', 1);
    pushInto(m, 'b', 2);
    pushInto(m, 'a', 3);
    expect(m.get('a')).toEqual([1, 3]);
    expect(m.get('b')).toEqual([2]);
  });

  it('mutates the bucket in place rather than replacing it', () => {
    // The whole point: the previous shape allocated a new array per append. A caller holding a
    // reference to the bucket (none do today) would see the append, and more importantly the cost
    // is O(1) rather than O(k) per call.
    const m = new Map<string, number[]>();
    pushInto(m, 'a', 1);
    const first = m.get('a');
    pushInto(m, 'a', 2);
    expect(m.get('a')).toBe(first);
  });
});
