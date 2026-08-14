// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Thresholds filter state (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import {
  defaultFilters,
  isAnyFiltered,
  readFilterParams,
  specColumns,
  writeFilterParams,
} from '../lib/columnFilter';
import { encodeCondition } from '../lib/filterCondition';
import { queryFor, thresholdFilters } from './thresholdQuery';

const t = ((k: string) => k) as unknown as TFunction;
const COLUMNS = specColumns(thresholdFilters(t));
/** The state the screen opens with — derived from the specs, exactly as the page derives it. */
const DEFAULTS = defaultFilters(COLUMNS);
const query = (over: Record<string, string> = {}) => queryFor(COLUMNS, { ...DEFAULTS, ...over });
/** What the page holds after the router hands it a query string. */
const read = (qs: string) => readFilterParams(COLUMNS, new URLSearchParams(qs));

describe('queryFor', () => {
  it('sends nothing at all when nothing is filtered', () => {
    expect(query()).toEqual({ q: undefined, scope_level: undefined, direction: undefined });
  });

  it('never sends an empty string, which the backend would reject', () => {
    // `buildUrl` drops undefined but keeps '', so `scope_level=` would reach the API as an unknown
    // level and 400 — a filter nobody set turning into an error.
    for (const v of Object.values(query({ q: '   ' }))) expect(v === '').toBe(false);
    expect(query({ q: '   ' }).q).toBeUndefined();
  });

  it('passes each filter through under the name the API takes', () => {
    expect(
      query({
        q: encodeCondition({ term: 'cpu', mode: 'contains', not: false }),
        scope_level: 'node',
        direction: 'below',
      }),
    ).toEqual({ q: 'cpu', scope_level: 'node', direction: 'below' });
  });

  it('trims the search term', () => {
    expect(query({ q: '  cpu_util  ' }).q).toBe('cpu_util');
  });

  it('drops a token the column does not offer instead of forwarding it', () => {
    // 🚨 The trap decision AA had to close. The URL is read without validation on purpose — a
    // stale bookmark must open the default view rather than render a control showing a value it
    // does not offer — so `?scope_level=galaxy` reaching the API is what `normalizeSets` prevents.
    expect(query({ scope_level: 'galaxy', direction: 'sideways' })).toEqual({
      q: undefined,
      scope_level: undefined,
      direction: undefined,
    });
    expect(query({ scope_level: 'galaxy,node' }).scope_level).toBe('node');
  });

  it('reads a set the operator typed in any order back in the spec order', () => {
    // The joined value is an effect dependency and a shared link: both need the same selection to
    // produce the same string however it was reached.
    expect(query({ scope_level: 'node,profile' }).scope_level).toBe(
      query({ scope_level: 'profile,node' }).scope_level,
    );
  });
});

describe('the empty state discriminator', () => {
  it('is false for the default view and flips for every column', () => {
    // ⚠️ Must not be replaced by a `rows.length` check: with the predicate in SQL, a filtered
    // query that legitimately returns zero is indistinguishable from an empty ruleset.
    expect(isAnyFiltered(COLUMNS, DEFAULTS)).toBe(false);
    for (const c of COLUMNS) {
      expect(isAnyFiltered(COLUMNS, { ...DEFAULTS, [c.key]: 'x' }), c.key).toBe(true);
    }
  });
});

describe('the URL codec', () => {
  // Now the shared one (`readFilterParams`/`writeFilterParams`), reached through `useFilterParams`.
  // The keys did not move: a column key **is** its query key, and these three were already named
  // after the query parameters this screen shipped with, so older bookmarks still resolve.
  it('reads an empty query as the default view', () => {
    expect(read('')).toEqual(DEFAULTS);
  });

  it('round-trips every field', () => {
    const all = {
      q: encodeCondition({ term: 'cpu_util', mode: 'contains', not: false }),
      scope_level: 'group',
      direction: 'above',
    };
    const params = new URLSearchParams();
    writeFilterParams(COLUMNS, params, { ...DEFAULTS, ...all });
    expect(read(params.toString())).toEqual({ ...DEFAULTS, ...all });
  });

  it('leaves no query string at all for the default view', () => {
    const params = new URLSearchParams('q=cpu&scope_level=node&direction=below');
    writeFilterParams(COLUMNS, params, DEFAULTS);
    expect(params.toString()).toBe('');
  });

  it('keeps an unknown value out of the request rather than out of the URL', () => {
    // The division of labour that changed with Inc.10: reading no longer drops the token (the
    // control shows nothing selected), and `queryFor` is what stops it reaching the API. Both
    // spellings end at the same place — the default view — and only one of them can also serve a
    // bookmark whose tokens this build has never heard of.
    expect(read('scope_level=galaxy').scope_level).toBe('galaxy');
    expect(queryFor(COLUMNS, read('scope_level=galaxy')).scope_level).toBeUndefined();
  });

  it('leaves query keys it does not own alone', () => {
    const params = new URLSearchParams('tab=rules&q=cpu');
    writeFilterParams(COLUMNS, params, DEFAULTS);
    expect(params.toString()).toBe('tab=rules');
  });
});
