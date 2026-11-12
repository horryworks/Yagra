// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { moveOnto, placeWithinGroup } from './reorder';

describe('moveOnto (ADR-255)', () => {
  const ids = ['a', 'b', 'c', 'd'];

  it('puts the dragged item where the one it was dropped on stands', () => {
    expect(moveOnto(ids, 'd', 'a')).toEqual(['d', 'a', 'b', 'c']);
    expect(moveOnto(ids, 'a', 'c')).toEqual(['b', 'c', 'a', 'd']);
    expect(moveOnto(ids, 'b', 'c')).toEqual(['a', 'c', 'b', 'd']);
  });

  it('says nothing moved for a drop on itself, outside the list, or an unknown id', () => {
    expect(moveOnto(ids, 'b', 'b')).toBeNull();
    expect(moveOnto(ids, 'b', null)).toBeNull();
    expect(moveOnto(ids, 'zz', 'a')).toBeNull();
    expect(moveOnto(ids, 'a', 'zz')).toBeNull();
  });

  it('never changes the list it was given', () => {
    const before = [...ids];
    moveOnto(ids, 'd', 'a');
    expect(ids).toEqual(before);
  });
});

describe('placeWithinGroup (ADR-205 Inc.2, ADR-255)', () => {
  // s = SNMP, h = HTTP: the HTTP rows must not move when the SNMP ones are reordered.
  const all = ['s1', 'h1', 's2', 'h2', 's3'];

  it('fills the group slots with the new order', () => {
    expect(placeWithinGroup(all, ['s3', 's1', 's2'])).toEqual(['s3', 'h1', 's1', 'h2', 's2']);
  });

  it('keeps every row of another group exactly where it stood', () => {
    const next = placeWithinGroup(all, ['s2', 's3', 's1']);
    expect(next[1]).toBe('h1');
    expect(next[3]).toBe('h2');
  });

  it('ignores an id the whole list does not hold, and leaves an unnamed member in its slot', () => {
    expect(placeWithinGroup(all, ['zz', 's2', 's1'])).toEqual(['s2', 'h1', 's1', 'h2', 's3']);
  });
});
