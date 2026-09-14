// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { CONFIRM_NAMES_MAX, deletedEverything, namesToConfirm } from './deleteNodes';

const named = (n: number) => Array.from({ length: n }, (_, i) => ({ name: `as${i + 1}` }));

describe('namesToConfirm', () => {
  it('names every node when the selection is short', () => {
    expect(namesToConfirm(named(3))).toEqual({ shown: ['as1', 'as2', 'as3'], more: 0 });
  });

  it('names the first ones and counts the rest past the cap', () => {
    const out = namesToConfirm(named(CONFIRM_NAMES_MAX + 52));
    expect(out.shown).toHaveLength(CONFIRM_NAMES_MAX);
    expect(out.shown[0]).toBe('as1');
    expect(out.more).toBe(52);
  });

  it('names exactly the cap without an "and 0 more"', () => {
    expect(namesToConfirm(named(CONFIRM_NAMES_MAX)).more).toBe(0);
  });
});

describe('deletedEverything', () => {
  it('is true only when every requested node went', () => {
    expect(deletedEverything({ requested: 62, deleted: 62 })).toBe(true);
    // A node already gone, or outside the caller's folders: the dialog must not claim it.
    expect(deletedEverything({ requested: 62, deleted: 61 })).toBe(false);
  });
});
