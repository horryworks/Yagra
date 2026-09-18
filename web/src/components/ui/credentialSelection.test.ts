import { describe, it, expect } from 'vitest';
import { toggleSelection } from './credentialSelection';

const OPTIONS = [{ id: 'a' }, { id: 'b' }, { id: 'c' }, { id: 'd' }];

describe('toggleSelection', () => {
  it('adds in the order the options are offered, not the order they were clicked', () => {
    // The bug, stated: ticking c, then a, then b used to send ['c','a','b'] while the chips
    // read "a b c" — and the first order is the one the sweep probes in (ADR-161).
    let sel: string[] = [];
    sel = toggleSelection(OPTIONS, sel, 'c');
    sel = toggleSelection(OPTIONS, sel, 'a');
    sel = toggleSelection(OPTIONS, sel, 'b');
    expect(sel).toEqual(['a', 'b', 'c']);
  });

  it('removes a selected id and leaves the rest in option order', () => {
    expect(toggleSelection(OPTIONS, ['a', 'b', 'c'], 'b')).toEqual(['a', 'c']);
  });

  it('round-trips: ticking then unticking returns the original selection', () => {
    const start = ['b', 'd'];
    expect(toggleSelection(OPTIONS, toggleSelection(OPTIONS, start, 'a'), 'a')).toEqual(start);
  });

  it('ignores an id that is not among the options rather than inventing one', () => {
    // A stale remembered id (ADR-134 決定 5 replays the last scan's credentials) must not be
    // resurrected into the request: `resolve_scan_credentials` answers 400 for an id that no
    // longer exists, which would fail the whole scan rather than one credential.
    expect(toggleSelection(OPTIONS, ['a'], 'gone')).toEqual(['a']);
  });

  it('drops ids no longer offered when anything is ticked', () => {
    expect(toggleSelection(OPTIONS, ['a', 'stale'], 'b')).toEqual(['a', 'b']);
  });
});
