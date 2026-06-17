// Pure-helper unit tests for RangeControl. Vitest runs in a node env (no DOM), so we cover only the
// exported pure helpers, not the component render. Importing the component module is safe: it merely
// defines functions/JSX (nothing renders here) and the CSS import is a no-op under Vitest.
//
// Every assertion is timezone-agnostic: the local-time helpers are exercised via round-trips and via
// inputs constructed with the same local Date fields they format back to.

import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  formatCompactRange,
  localInputToUnix,
  rangeInputsValid,
  resolveRange,
  unixToLocalInput,
} from './RangeControl';

describe('resolveRange', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('returns absolute bounds unchanged', () => {
    expect(resolveRange({ kind: 'absolute', from: 100, to: 200 })).toEqual({ from: 100, to: 200 });
  });

  it('resolves a relative range against the current time', () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date('2026-06-17T12:00:00Z'));
    const now = Math.floor(Date.now() / 1000);
    expect(resolveRange({ kind: 'relative', secs: 3600 })).toEqual({ from: now - 3600, to: now });
  });
});

describe('datetime-local conversion', () => {
  it('round-trips unix → input → unix at minute resolution', () => {
    for (const base of [0, 1_700_000_000, 1_750_000_000]) {
      const s = base - (base % 60); // datetime-local has no seconds; align first
      expect(localInputToUnix(unixToLocalInput(s))).toBe(s);
    }
  });

  it('produces a YYYY-MM-DDTHH:MM shaped string', () => {
    expect(unixToLocalInput(1_750_000_000)).toMatch(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}$/);
  });

  it('returns null for empty or unparseable input', () => {
    expect(localInputToUnix('')).toBeNull();
    expect(localInputToUnix('not-a-date')).toBeNull();
  });
});

describe('formatCompactRange', () => {
  it('formats local M/D HH:MM–M/D HH:MM with an en-dash and zero-padded time', () => {
    const from = Math.floor(new Date(2026, 5, 12, 9, 5).getTime() / 1000); // Jun 12 09:05 local
    const to = Math.floor(new Date(2026, 5, 17, 18, 30).getTime() / 1000); // Jun 17 18:30 local
    expect(formatCompactRange(from, to)).toBe('6/12 09:05–6/17 18:30');
  });
});

describe('rangeInputsValid', () => {
  it('is true only when both parse and from < to', () => {
    expect(rangeInputsValid('2026-06-12T12:00', '2026-06-17T09:30')).toBe(true);
  });

  it('is false for empty, unparseable, equal, or inverted inputs', () => {
    expect(rangeInputsValid('', '2026-06-17T09:30')).toBe(false);
    expect(rangeInputsValid('2026-06-12T12:00', '')).toBe(false);
    expect(rangeInputsValid('garbage', '2026-06-17T09:30')).toBe(false);
    expect(rangeInputsValid('2026-06-17T09:30', '2026-06-17T09:30')).toBe(false);
    expect(rangeInputsValid('2026-06-18T09:30', '2026-06-17T09:30')).toBe(false);
  });
});
