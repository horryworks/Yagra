// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Threshold rules view and filter state (no DOM — Vitest node env).

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
import {
  SCOPE_TABS,
  queryFor,
  ruleViewFrom,
  startFormFor,
  targetKind,
  thresholdFilters,
  thresholdRulesHref,
  writeRuleView,
  type RuleView,
} from './thresholdQuery';
import { SCOPE_LEVELS } from '../types/api';

const t = ((k: string) => k) as unknown as TFunction;
const COLUMNS = specColumns(thresholdFilters(t));
/** The state the screen opens with — derived from the specs, exactly as the page derives it. */
const DEFAULTS = defaultFilters(COLUMNS);
const ALL: RuleView = { tab: 'all', target: null, check: null };
const ID = '0e7a3b52-6a0f-4f4f-9d0e-2b7c1a9d4e11';
const query = (over: Record<string, string> = {}, view: RuleView = ALL) =>
  queryFor(COLUMNS, { ...DEFAULTS, ...over }, view);
/** What the page holds after the router hands it a query string. */
const read = (qs: string) => readFilterParams(COLUMNS, new URLSearchParams(qs));
const view = (qs: string) => ruleViewFrom(new URLSearchParams(qs));

describe('the level tabs', () => {
  it('offer every level, after All', () => {
    // Derived from SCOPE_LEVELS, so a new level gets a tab rather than being left out.
    expect(SCOPE_TABS).toEqual(['all', ...SCOPE_LEVELS]);
  });

  it('give a target picker to exactly the levels that name things', () => {
    expect(SCOPE_TABS.filter((tab) => targetKind(tab))).toEqual(['profile', 'group_id', 'node']);
  });
});

describe('the view in the URL', () => {
  it('opens All for an empty, unknown or multi-valued level', () => {
    for (const qs of ['', 'scope_level=galaxy', 'scope_level=global,profile']) {
      expect(view(qs), qs).toEqual(ALL);
    }
  });

  it('keeps a target only on a tab that names that kind of thing, and only a uuid', () => {
    expect(view(`scope_level=profile&scope_id=${ID}`)).toEqual({ ...ALL, tab: 'profile', target: ID });
    expect(view(`scope_level=global&scope_id=${ID}`).target).toBeNull();
    expect(view('scope_level=node&scope_id=sw-01').target).toBeNull();
  });

  it('reads a node to check, and drops one that is not an id', () => {
    expect(view(`check=${ID}`).check).toBe(ID);
    expect(view('check=sw-01').check).toBeNull();
  });

  it('round-trips, and leaves no key for what is not set', () => {
    const v: RuleView = { tab: 'group_id', target: ID, check: null };
    const params = new URLSearchParams('q=cpu');
    writeRuleView(params, v);
    expect(ruleViewFrom(params)).toEqual(v);
    writeRuleView(params, ALL);
    expect(params.toString()).toBe('q=cpu');
  });

  it('builds the links other screens use', () => {
    expect(thresholdRulesHref({})).toBe('/alerts/rules');
    expect(thresholdRulesHref({ tab: 'profile', target: ID })).toBe(
      `/alerts/rules?scope_level=profile&scope_id=${ID}`,
    );
    expect(thresholdRulesHref({ check: ID })).toBe(`/alerts/rules?check=${ID}`);
  });
});

describe('the add dialog from a tab', () => {
  it('opens at the tab level with the chosen target picked', () => {
    const f = startFormFor({ tab: 'node', target: ID, check: null });
    expect(f?.level).toBe('node');
    expect(f?.scopeIds).toEqual([ID]);
    expect(startFormFor({ tab: 'global', target: null, check: null })?.scopeIds).toEqual([]);
  });

  it('leaves the dialog to its own default on All', () => {
    expect(startFormFor(ALL)).toBeUndefined();
  });
});

describe('queryFor', () => {
  it('asks for every level, ports included, when nothing is chosen', () => {
    expect(query()).toEqual({
      q: undefined,
      scope_level: undefined,
      scope_id: undefined,
      direction: undefined,
    });
  });

  it('sends the tab and its target', () => {
    expect(query({}, { tab: 'profile', target: ID, check: null })).toMatchObject({
      scope_level: 'profile',
      scope_id: ID,
    });
  });

  it('never sends an empty string, which the backend would reject', () => {
    // `buildUrl` drops undefined but keeps '', so `direction=` would reach the API as an unknown
    // value and 400 — a filter nobody set turning into an error.
    for (const v of Object.values(query({ q: '   ' }))) expect(v === '').toBe(false);
    expect(query({ q: '   ' }).q).toBeUndefined();
  });

  it('passes each filter through under the name the API takes', () => {
    expect(
      query({
        q: encodeCondition({ term: 'cpu', mode: 'contains', not: false }),
        direction: 'below',
      }),
    ).toMatchObject({ q: 'cpu', direction: 'below' });
  });

  it('trims the search term', () => {
    expect(query({ q: '  cpu_util  ' }).q).toBe('cpu_util');
  });

  it('drops a token the column does not offer instead of forwarding it', () => {
    expect(query({ direction: 'sideways' }).direction).toBeUndefined();
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
  it('reads an empty query as the default view', () => {
    expect(read('')).toEqual(DEFAULTS);
  });

  it('round-trips every field', () => {
    const all = {
      q: encodeCondition({ term: 'cpu_util', mode: 'contains', not: false }),
      direction: 'above',
    };
    const params = new URLSearchParams();
    writeFilterParams(COLUMNS, params, { ...DEFAULTS, ...all });
    expect(read(params.toString())).toEqual({ ...DEFAULTS, ...all });
  });

  it('leaves the view keys alone', () => {
    // The tabs and the columns share one query string; writing one must not erase the other.
    const params = new URLSearchParams(`scope_level=node&scope_id=${ID}&q=cpu`);
    writeFilterParams(COLUMNS, params, DEFAULTS);
    expect(params.toString()).toBe(`scope_level=node&scope_id=${ID}`);
  });
});
