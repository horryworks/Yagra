// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { keepOrder } from './stableOrder';

interface Row {
  id: string;
  n: number;
}
const key = (r: Row) => r.id;
const ids = (rows: Row[]) => rows.map(key);

describe('keepOrder', () => {
  it('keeps a row where it was when the new answer sorts it elsewhere', () => {
    // Missing IP prefixes: site b had the most gaps; marking one drops it below c.
    const answer = [
      { id: 'c', n: 4 },
      { id: 'b', n: 3 },
      { id: 'a', n: 1 },
    ];
    expect(ids(keepOrder(['b', 'c', 'a'], answer, key))).toEqual(['b', 'c', 'a']);
  });

  it('takes every row from the new answer, not from the old one', () => {
    const answer = [
      { id: 'c', n: 4 },
      { id: 'b', n: 3 },
    ];
    const out = keepOrder(['b', 'c'], answer, key);
    expect(out[0]).toBe(answer[1]);
    expect(out[0].n).toBe(3);
  });

  it('puts a row it has not seen after the ones it has, in the answer order', () => {
    const answer = [
      { id: 'y', n: 0 },
      { id: 'a', n: 0 },
      { id: 'x', n: 0 },
      { id: 'b', n: 0 },
    ];
    expect(ids(keepOrder(['b', 'a'], answer, key))).toEqual(['b', 'a', 'y', 'x']);
  });

  it('drops a row the new answer no longer has', () => {
    const answer = [
      { id: 'a', n: 0 },
      { id: 'c', n: 0 },
    ];
    expect(ids(keepOrder(['c', 'b', 'a'], answer, key))).toEqual(['c', 'a']);
  });

  it('is the answer as it is when nothing was drawn before', () => {
    const answer = [
      { id: 'b', n: 0 },
      { id: 'a', n: 0 },
    ];
    expect(ids(keepOrder([], answer, key))).toEqual(['b', 'a']);
    expect(keepOrder(['a'], [], key)).toEqual([]);
  });
});
