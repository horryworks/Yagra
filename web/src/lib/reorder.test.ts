// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { moveBy, moveWithinGroup } from './reorder';

describe('moveBy', () => {
  it('swaps an item with its neighbour and moves nothing else', () => {
    expect(moveBy(['a', 'b', 'c', 'd'], 2, -1)).toEqual(['a', 'c', 'b', 'd']);
    expect(moveBy(['a', 'b', 'c', 'd'], 1, 1)).toEqual(['a', 'c', 'b', 'd']);
  });

  it('leaves the list as it was at either end, or for an index it does not hold', () => {
    expect(moveBy(['a', 'b'], 0, -1)).toEqual(['a', 'b']);
    expect(moveBy(['a', 'b'], 1, 1)).toEqual(['a', 'b']);
    expect(moveBy(['a', 'b'], -1, 1)).toEqual(['a', 'b']);
    expect(moveBy(['a', 'b'], 5, -1)).toEqual(['a', 'b']);
    expect(moveBy([], 0, 1)).toEqual([]);
  });

  it('never changes the list it was given', () => {
    const items = ['a', 'b', 'c'];
    moveBy(items, 1, 1);
    expect(items).toEqual(['a', 'b', 'c']);
  });
});

describe('moveWithinGroup', () => {
  // s* are one tab's rows, h* another's, interleaved as the stored order may have them.
  const all = ['s1', 'h1', 's2', 'h2', 's3'];
  const snmp = ['s1', 's2', 's3'];

  it('swaps with the next member of the group, over rows of other groups', () => {
    expect(moveWithinGroup(all, snmp, 's2', -1)).toEqual(['s2', 'h1', 's1', 'h2', 's3']);
    expect(moveWithinGroup(all, snmp, 's2', 1)).toEqual(['s1', 'h1', 's3', 'h2', 's2']);
  });

  it('keeps every row of another group exactly where it stood', () => {
    const moved = moveWithinGroup(all, snmp, 's3', -1);
    expect(moved.indexOf('h1')).toBe(1);
    expect(moved.indexOf('h2')).toBe(3);
    expect([...moved].sort()).toEqual([...all].sort());
  });

  it('does nothing at the ends of the group', () => {
    expect(moveWithinGroup(all, snmp, 's1', -1)).toEqual(all);
    expect(moveWithinGroup(all, snmp, 's3', 1)).toEqual(all);
  });
});
