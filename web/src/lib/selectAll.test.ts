// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { nextSelection, selectAllState, toggleOne } from './selectAll';

describe('selectAllState', () => {
  it('reads none, some and all over the visible rows', () => {
    expect(selectAllState(['a', 'b'], new Set())).toBe('none');
    expect(selectAllState(['a', 'b'], new Set(['a']))).toBe('some');
    expect(selectAllState(['a', 'b'], new Set(['a', 'b']))).toBe('all');
  });

  it('ignores a selected row the filter hides', () => {
    expect(selectAllState(['a', 'b'], new Set(['z']))).toBe('none');
    expect(selectAllState(['a'], new Set(['a', 'z']))).toBe('all');
  });

  it('is none when nothing can be ticked', () => {
    expect(selectAllState([], new Set(['a']))).toBe('none');
  });
});

describe('nextSelection', () => {
  it('selects every visible row from none or some, keeping hidden selections', () => {
    expect([...nextSelection(['a', 'b'], new Set())].sort()).toEqual(['a', 'b']);
    expect([...nextSelection(['a', 'b'], new Set(['a', 'z']))].sort()).toEqual(['a', 'b', 'z']);
  });

  it('clears everything, hidden rows included, from all', () => {
    expect([...nextSelection(['a', 'b'], new Set(['a', 'b', 'z']))]).toEqual([]);
  });

  it('never selects a row that is not visible', () => {
    expect(nextSelection(['a'], new Set()).has('b')).toBe(false);
  });

  it('changes nothing when no row can be ticked', () => {
    expect([...nextSelection([], new Set(['z']))]).toEqual(['z']);
  });
});

describe('toggleOne', () => {
  it('adds and removes one key without touching the others', () => {
    expect([...toggleOne(new Set(['a']), 'b', true)].sort()).toEqual(['a', 'b']);
    expect([...toggleOne(new Set(['a', 'b']), 'a', false)]).toEqual(['b']);
  });
});
