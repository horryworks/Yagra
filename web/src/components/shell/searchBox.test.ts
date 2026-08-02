// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  MAX_RESULTS,
  isCapped,
  keyAction,
  moveActive,
  resultRoute,
  shouldFocusOnSlash,
} from './searchBox';

describe('keyAction', () => {
  it('maps the navigation keys and ignores everything else', () => {
    expect(keyAction('ArrowDown')).toBe('down');
    expect(keyAction('ArrowUp')).toBe('up');
    expect(keyAction('Enter')).toBe('open');
    expect(keyAction('Escape')).toBe('close');
    expect(keyAction('a')).toBeNull();
    expect(keyAction('Tab')).toBeNull();
  });
});

describe('moveActive', () => {
  it('clamps to the list rather than wrapping', () => {
    expect(moveActive(0, 1, 3)).toBe(1);
    expect(moveActive(2, 1, 3)).toBe(2);
    expect(moveActive(0, -1, 3)).toBe(0);
  });

  it('stays at 0 when there is nothing to highlight', () => {
    expect(moveActive(0, 1, 0)).toBe(0);
    expect(moveActive(5, -1, 0)).toBe(0);
  });
});

describe('resultRoute', () => {
  it('navigates to the node detail page', () => {
    expect(resultRoute({ id: 'abc-123' })).toBe('/nodes/abc-123');
  });
});

describe('isCapped', () => {
  it('is true only once the server returned a full page', () => {
    expect(isCapped(MAX_RESULTS - 1)).toBe(false);
    expect(isCapped(MAX_RESULTS)).toBe(true);
  });
});

describe('shouldFocusOnSlash', () => {
  it('does not steal "/" from a field the operator is typing in', () => {
    // Without this, "/" becomes untypeable in every form on every page — a worse regression than
    // having no shortcut at all.
    expect(shouldFocusOnSlash('INPUT', false)).toBe(false);
    expect(shouldFocusOnSlash('TEXTAREA', false)).toBe(false);
    expect(shouldFocusOnSlash('SELECT', false)).toBe(false);
    expect(shouldFocusOnSlash('DIV', true)).toBe(false);
  });

  it('takes "/" when focus is on the page rather than in a field', () => {
    expect(shouldFocusOnSlash('BODY', false)).toBe(true);
    expect(shouldFocusOnSlash('BUTTON', false)).toBe(true);
    expect(shouldFocusOnSlash(undefined, false)).toBe(true);
  });

  it('is case-insensitive about the tag name', () => {
    expect(shouldFocusOnSlash('input', false)).toBe(false);
  });
});
