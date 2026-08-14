// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { filterRowForced, filterRowVisible } from './filterRow';

describe('filterRowVisible', () => {
  it('never draws the row on a phone, whatever the preference says', () => {
    // The mobile half of the either/or. Both of the other inputs are set to the values that would
    // show the row on a desktop, so this fails if the viewport check is ever dropped.
    expect(filterRowVisible('mobile', true, 3)).toBe(false);
    expect(filterRowVisible('mobile', false, 0)).toBe(false);
  });

  it('is closed by default on a desktop — the Inc.9 inversion', () => {
    expect(filterRowVisible('desktop', false, 0)).toBe(false);
  });

  it('draws the row when the operator opened it', () => {
    expect(filterRowVisible('desktop', true, 0)).toBe(true);
  });

  // The half that cannot be expressed as "the operator's choice": arriving on a shared URL that
  // already carries a filter must show the controls that produced the narrowing.
  it('draws the row while a filter is narrowing the list, even when closed', () => {
    expect(filterRowVisible('desktop', false, 1)).toBe(true);
    expect(filterRowVisible('desktop', false, 2)).toBe(true);
  });

  it('states forcing separately, because the toggle reads the same fact', () => {
    // `filterRowForced` is what makes the button `aria-disabled`. Asserted on its own so that a
    // change which unhooked the two — a row that stays open while the button says it can close —
    // fails here rather than only in the browser.
    expect(filterRowForced(0)).toBe(false);
    expect(filterRowForced(1)).toBe(true);
  });
});
