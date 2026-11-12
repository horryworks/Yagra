// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { moveOnto } from '../../lib/reorder';
import { arrangeRows, selectionOf, toggleRow } from './credentialSelection';

const OPTIONS = [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }];

describe('arrangeRows (ADR-205 Inc.7)', () => {
  it('first draws the selection on top in its order, the rest after in the order offered', () => {
    expect(arrangeRows(OPTIONS, ['c', 'a'], null)).toEqual(['c', 'a', 'b', 'd']);
    // Nothing ticked: the order offered, so a screen that starts empty can still be arranged.
    expect(arrangeRows(OPTIONS, [], null)).toEqual(['a', 'b', 'c', 'd']);
  });

  it('keeps every row where it was once drawn', () => {
    const rows = ['c', 'a', 'b', 'd'];
    expect(arrangeRows(OPTIONS, ['c', 'a'], rows)).toEqual(rows);
    // Unticking 'c' leaves it in place: the selection is now just 'a'.
    expect(arrangeRows(OPTIONS, ['a'], rows)).toEqual(rows);
    // Ticking 'd' leaves it at the bottom, and it is tried last.
    expect(arrangeRows(OPTIONS, ['c', 'a', 'd'], rows)).toEqual(rows);
  });

  it('puts an order set from outside into the ticked places without moving an unticked row', () => {
    // "Select all" sends every login in the try order; the rows follow it.
    expect(arrangeRows(OPTIONS, ['a', 'b', 'c', 'd'], ['d', 'b', 'c', 'a'])).toEqual(['a', 'b', 'c', 'd']);
    // 'b' is unticked and stays second; the two ticked places take the new order.
    expect(arrangeRows(OPTIONS, ['a', 'c'], ['c', 'b', 'a', 'd'])).toEqual(['a', 'b', 'c', 'd']);
  });

  it('adds a newly offered credential at the end and drops one no longer offered', () => {
    expect(arrangeRows([...OPTIONS, { id: 'e' }], ['b'], ['b', 'a', 'c', 'd'])).toEqual(['b', 'a', 'c', 'd', 'e']);
    expect(arrangeRows(OPTIONS.slice(1), ['b', 'a'], ['b', 'a', 'c', 'd'])).toEqual(['b', 'c', 'd']);
  });

  it('draws each offered credential once, whatever the selection holds', () => {
    const rows = arrangeRows(OPTIONS, ['b', 'b', 'zz'], ['a', 'b', 'c', 'd']);
    expect([...rows].sort()).toEqual(['a', 'b', 'c', 'd']);
  });
});

describe('toggleRow and a dragged row (ADR-205 Inc.7, ADR-255)', () => {
  const rows = ['a', 'b', 'c', 'd'];

  it('ticks and unticks in place: the selection is the ticked rows top to bottom', () => {
    expect(toggleRow(rows, [], 'c')).toEqual(['c']);
    expect(toggleRow(rows, ['c'], 'a')).toEqual(['a', 'c']);
    expect(toggleRow(rows, ['a', 'c'], 'a')).toEqual(['c']);
    // Round trip: ticking then unticking gives back what was there.
    expect(toggleRow(rows, toggleRow(rows, ['b'], 'd'), 'd')).toEqual(['b']);
  });

  it('dragging a ticked row past another ticked one changes the try order, past an unticked one does not', () => {
    const ticked = new Set(['a', 'c']);
    expect(selectionOf(moveOnto(['a', 'c', 'b'], 'c', 'a') ?? [], ticked)).toEqual(['c', 'a']);
    expect(selectionOf(moveOnto(['a', 'b', 'c'], 'c', 'b') ?? [], ticked)).toEqual(['a', 'c']);
  });
});
