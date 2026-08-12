// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the shared filter-query helpers (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { isFiltered, sinceIso, unset } from './filterQuery';

describe('unset', () => {
  it('drops the empty sentinel so it never reaches the query string', () => {
    // `buildUrl` keeps '' and drops undefined, so `severity=` would reach the backend as an empty
    // string and be rejected as an unknown severity — a filter nobody set turning into a 400.
    expect(unset('')).toBeUndefined();
    expect(unset('   ')).toBeUndefined();
    expect(unset(null)).toBeUndefined();
    expect(unset(undefined)).toBeUndefined();
  });

  it('keeps a real value, trimmed', () => {
    expect(unset('critical')).toBe('critical');
    expect(unset('  critical  ')).toBe('critical');
  });
});

describe('sinceIso', () => {
  const NOW = Date.parse('2026-08-12T00:00:00.000Z');

  it('returns no bound for "all time"', () => {
    expect(sinceIso(null, NOW)).toBeUndefined();
  });

  it('subtracts the window from the supplied clock', () => {
    expect(sinceIso(86_400, NOW)).toBe('2026-08-11T00:00:00.000Z');
    expect(sinceIso(7 * 86_400, NOW)).toBe('2026-08-05T00:00:00.000Z');
  });
});

describe('isFiltered', () => {
  const DEFAULTS = { severity: '', state: '', resolved: '', range: '7d', nodeId: '', flapping: false };

  it('is false for the defaults', () => {
    expect(isFiltered({ ...DEFAULTS }, DEFAULTS)).toBe(false);
  });

  it('flips for EVERY field, including ones added later', () => {
    // The point of deriving this from the defaults rather than writing the disjunction by hand: a
    // filter added without its clause would make the empty state say "there is nothing here at all"
    // while a filter is hiding the rows. This loop cannot be out of date.
    for (const key of Object.keys(DEFAULTS) as (keyof typeof DEFAULTS)[]) {
      const changed = { ...DEFAULTS, [key]: typeof DEFAULTS[key] === 'boolean' ? true : 'x' };
      expect(isFiltered(changed, DEFAULTS), `${String(key)} did not register as a filter`).toBe(true);
    }
  });

  it('treats a non-empty default as unfiltered, and its absence as filtered', () => {
    // A bounded range is the default view, so selecting it is not "narrowing"; widening to all time
    // is a deliberate change and should read as one.
    expect(isFiltered({ ...DEFAULTS, range: '7d' }, DEFAULTS)).toBe(false);
    expect(isFiltered({ ...DEFAULTS, range: 'all' }, DEFAULTS)).toBe(true);
  });
});
