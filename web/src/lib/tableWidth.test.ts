// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { columnMinWidth, minTableWidth, FLEX_MIN_PX } from './tableWidth';

describe('columnMinWidth', () => {
  it('reads a fixed track at face value', () => {
    expect(columnMinWidth('120px')).toBe(120);
    expect(columnMinWidth('58px')).toBe(58);
    expect(columnMinWidth(' 210px ')).toBe(210);
  });

  it('gives a flexible track the floor', () => {
    expect(columnMinWidth('1fr')).toBe(FLEX_MIN_PX);
    expect(columnMinWidth('1.2fr')).toBe(FLEX_MIN_PX);
    expect(columnMinWidth('2fr')).toBe(FLEX_MIN_PX);
    expect(columnMinWidth(undefined)).toBe(FLEX_MIN_PX);
  });

  it("prefers the author's own floor when the column declares one", () => {
    // All four spellings below exist in the tree today — they are the local workarounds people
    // reached for when a table clipped, and this is what makes them mean something.
    expect(columnMinWidth('minmax(220px, 2fr)')).toBe(220);
    expect(columnMinWidth('minmax(150px, 1.2fr)')).toBe(150);
    expect(columnMinWidth('minmax(140px, 1fr)')).toBe(140);
    expect(columnMinWidth('minmax(130px, 1fr)')).toBe(130);
  });

  it('falls back to the floor rather than to zero on anything it does not understand', () => {
    // The failure this guards is the one the whole module exists for: a width that measures as 0
    // under-reports the table, and the difference is clipped with no scrollbar and no error.
    for (const odd of ['auto', 'min-content', 'max-content', '10%', 'minmax(min-content, 1fr)', '']) {
      expect(columnMinWidth(odd), odd).toBe(FLEX_MIN_PX);
    }
  });
});

describe('minTableWidth', () => {
  it('is zero for no columns, so the caller can skip the style', () => {
    expect(minTableWidth([])).toBe(0);
  });

  it('reproduces the three tables that were being clipped', () => {
    // The measured `gridTemplateColumns` from the browser on 2026-08-14, minus the collapsed
    // flexible track (which is what this function is putting back). `.dt` was 1010px wide.
    const pollers = [
      { width: '1.2fr' },
      { width: '120px' },
      { width: '108px' },
      { width: '150px' },
      { width: '150px' },
      { width: '82px' },
      { width: '64px' },
      { width: '64px' },
      { width: '64px' },
      { width: '96px' },
      { width: '112px' },
      { width: '130px' },
      { width: '58px' },
    ];
    expect(minTableWidth(pollers)).toBe(1198 + FLEX_MIN_PX);
    expect(minTableWidth(pollers)).toBeGreaterThan(1010);
  });

  it('leaves a table that already fits below the pane it fits in', () => {
    const narrow = [{ width: '1fr' }, { width: '120px' }, { width: '96px' }];
    expect(minTableWidth(narrow)).toBe(FLEX_MIN_PX + 216);
    expect(minTableWidth(narrow)).toBeLessThan(1010);
  });
});
