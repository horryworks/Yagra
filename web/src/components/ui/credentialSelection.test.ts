// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { moveSelected, splitSelection, toggleSelection } from './credentialSelection';

const OPTIONS = [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }];

describe('toggleSelection', () => {
  it('adds at the end, so the order is the one the operator built (ADR-205 Inc.2)', () => {
    let sel: string[] = [];
    sel = toggleSelection(OPTIONS, sel, 'c');
    sel = toggleSelection(OPTIONS, sel, 'a');
    sel = toggleSelection(OPTIONS, sel, 'b');
    expect(sel).toEqual(['c', 'a', 'b']);
  });

  it('removes a selected id and closes the gap, keeping the rest in their order', () => {
    expect(toggleSelection(OPTIONS, ['c', 'a', 'b'], 'a')).toEqual(['c', 'b']);
  });

  it('round-trips: ticking then unticking returns the original selection', () => {
    const start = ['d', 'b'];
    expect(toggleSelection(OPTIONS, toggleSelection(OPTIONS, start, 'a'), 'a')).toEqual(start);
  });

  it('ignores an id that is not among the options rather than inventing one', () => {
    // A stale remembered id (ADR-134 decision 5 replays the last scan's credentials) must not be
    // resurrected into the request: `resolve_scan_credentials` answers 400 for an id that no
    // longer exists, which would fail the whole scan rather than one credential.
    expect(toggleSelection(OPTIONS, ['a'], 'gone')).toEqual(['a']);
  });

  it('drops ids no longer offered when anything is ticked', () => {
    expect(toggleSelection(OPTIONS, ['b', 'stale', 'a'], 'c')).toEqual(['b', 'a', 'c']);
  });
});

describe('moveSelected', () => {
  it('moves one credential by one place and nothing else', () => {
    expect(moveSelected(['a', 'b', 'c'], 'c', -1)).toEqual(['a', 'c', 'b']);
    expect(moveSelected(['a', 'b', 'c'], 'a', 1)).toEqual(['b', 'a', 'c']);
  });

  it('does nothing at either end or for an id not selected', () => {
    expect(moveSelected(['a', 'b'], 'a', -1)).toEqual(['a', 'b']);
    expect(moveSelected(['a', 'b'], 'b', 1)).toEqual(['a', 'b']);
    expect(moveSelected(['a', 'b'], 'x', 1)).toEqual(['a', 'b']);
  });
});

describe('splitSelection (ADR-205 Inc.3)', () => {
  const ids = (xs: { id: string }[]) => xs.map((x) => x.id);

  it('lists the used ones in the try order and the rest in the order offered', () => {
    const { used, unused } = splitSelection(OPTIONS, ['c', 'a']);
    expect(ids(used)).toEqual(['c', 'a']);
    expect(ids(unused)).toEqual(['b', 'd']);
  });

  it('shows every offered credential exactly once', () => {
    const { used, unused } = splitSelection(OPTIONS, ['d', 'b', 'd']);
    expect([...ids(used), ...ids(unused)].sort()).toEqual(['a', 'b', 'c', 'd']);
    expect(ids(used)).toEqual(['d', 'b']);
  });

  it('draws nothing for an id that is no longer offered', () => {
    const { used, unused } = splitSelection(OPTIONS, ['gone', 'a']);
    expect(ids(used)).toEqual(['a']);
    expect(ids(unused)).toEqual(['b', 'c', 'd']);
  });

  it('with nothing selected, everything is unused', () => {
    expect(ids(splitSelection(OPTIONS, []).unused)).toEqual(['a', 'b', 'c', 'd']);
    expect(splitSelection([], ['a'])).toEqual({ used: [], unused: [] });
  });
});
