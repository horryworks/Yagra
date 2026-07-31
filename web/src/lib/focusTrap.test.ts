// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { trapTarget } from './focusTrap';

// Strings stand in for elements — trapTarget only ever compares identity and position.
const ITEMS = ['first', 'middle', 'last'] as const;

describe('trapTarget', () => {
  it('leaves the interior alone', () => {
    // The browser's own Tab order is correct everywhere except the two edges; intervening in the
    // middle would break Shift+Tab and any custom tab order the dialog's content sets up.
    expect(trapTarget(ITEMS, 'middle', false)).toBeNull();
    expect(trapTarget(ITEMS, 'middle', true)).toBeNull();
    expect(trapTarget(ITEMS, 'first', false)).toBeNull();
    expect(trapTarget(ITEMS, 'last', true)).toBeNull();
  });

  it('wraps at both edges', () => {
    expect(trapTarget(ITEMS, 'last', false)).toBe('first');
    expect(trapTarget(ITEMS, 'first', true)).toBe('last');
  });

  it('pulls focus back in when it is not on a focusable inside the overlay', () => {
    // This is the state a dialog opens in: focus sits on the container, which is tabindex="-1" and
    // therefore absent from the list. Shift+Tab must reach the last control, not the page behind.
    expect(trapTarget(ITEMS, null, false)).toBe('first');
    expect(trapTarget(ITEMS, null, true)).toBe('last');
    expect(trapTarget(ITEMS, 'somewhere-else', true)).toBe('last');
  });

  it('declines to act when there is nothing to focus', () => {
    // An empty dialog: returning an element here would mean focusing undefined.
    expect(trapTarget([], null, false)).toBeNull();
    expect(trapTarget([], 'anything', true)).toBeNull();
  });

  it('wraps a single focusable onto itself', () => {
    // A confirmation dialog stripped to one button still must not leak focus to the page.
    expect(trapTarget(['only'], 'only', false)).toBe('only');
    expect(trapTarget(['only'], 'only', true)).toBe('only');
  });
});
