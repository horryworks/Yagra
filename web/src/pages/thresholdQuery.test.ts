// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Thresholds filter state (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import { DIRECTIONS, SCOPE_LEVELS } from '../types/api';
import {
  DEFAULT_THRESHOLD_FILTERS,
  isFiltered,
  queryFor,
  readFilters,
  writeFilters,
  type ThresholdFilters,
} from './thresholdQuery';

const read = (qs: string) => readFilters(new URLSearchParams(qs), SCOPE_LEVELS, DIRECTIONS);
const f = (over: Partial<ThresholdFilters>): ThresholdFilters => ({
  ...DEFAULT_THRESHOLD_FILTERS,
  ...over,
});

describe('queryFor', () => {
  it('sends nothing at all when nothing is filtered', () => {
    expect(queryFor(DEFAULT_THRESHOLD_FILTERS)).toEqual({
      q: undefined,
      scope_level: undefined,
      direction: undefined,
    });
  });

  it('never sends an empty string, which the backend would reject', () => {
    // `buildUrl` drops undefined but keeps '', so `scope_level=` would reach the API as an unknown
    // level and 400 — a filter nobody set turning into an error.
    for (const v of Object.values(queryFor(f({ q: '   ' })))) {
      expect(v === '' ).toBe(false);
    }
    expect(queryFor(f({ q: '   ' })).q).toBeUndefined();
  });

  it('passes each filter through under the name the API takes', () => {
    expect(queryFor(f({ q: 'cpu', scopeLevel: 'node', direction: 'below' }))).toEqual({
      q: 'cpu',
      scope_level: 'node',
      direction: 'below',
    });
  });

  it('trims the search term', () => {
    expect(queryFor(f({ q: '  cpu_util  ' })).q).toBe('cpu_util');
  });
});

describe('isFiltered', () => {
  it('is false for the default view and flips for every field', () => {
    // Driven off the defaults rather than a hand-written list: a filter added without its clause
    // would make the screen say "no rules yet" while a filter is hiding them — and on this table
    // that reads as "nothing is being alerted on".
    expect(isFiltered(DEFAULT_THRESHOLD_FILTERS)).toBe(false);
    const sample: ThresholdFilters = { q: 'cpu', scopeLevel: 'node', direction: 'below' };
    for (const key of Object.keys(DEFAULT_THRESHOLD_FILTERS) as (keyof ThresholdFilters)[]) {
      expect(isFiltered(f({ [key]: sample[key] }))).toBe(true);
    }
  });
});

describe('the URL codec', () => {
  it('reads an empty query as the default view', () => {
    expect(read('')).toEqual(DEFAULT_THRESHOLD_FILTERS);
  });

  it('round-trips every field', () => {
    const all: ThresholdFilters = { q: 'cpu_util', scopeLevel: 'group', direction: 'above' };
    const params = new URLSearchParams();
    writeFilters(params, all);
    expect(read(params.toString())).toEqual(all);
  });

  it('leaves no query string at all for the default view', () => {
    const params = new URLSearchParams('q=cpu&scope_level=node&direction=below');
    writeFilters(params, DEFAULT_THRESHOLD_FILTERS);
    expect(params.toString()).toBe('');
  });

  it('falls back instead of rejecting an unknown value', () => {
    // A stale bookmark must not render a control whose value is not one of its own options — the
    // opposite of the API edge, where an unknown token is a 400 so a typo cannot widen a search.
    expect(read('scope_level=galaxy&direction=sideways')).toEqual(DEFAULT_THRESHOLD_FILTERS);
  });

  it('leaves query keys it does not own alone', () => {
    const params = new URLSearchParams('tab=rules&q=cpu');
    writeFilters(params, DEFAULT_THRESHOLD_FILTERS);
    expect(params.toString()).toBe('tab=rules');
  });
});
