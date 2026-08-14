// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Metric sets / Device profiles filter rows (no DOM — Vitest node env).

import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import type { CollectionTemplate, ProfileSummary } from '../types/api';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { facetCounts } from '../lib/filterCounts';
import {
  OTHER_CATEGORY,
  PROFILE_INTERVAL,
  profileCategoryColumns,
  profileColumns,
  profileFilterLabels,
  setColumns,
  setFilterLabels,
} from './monitoringConfigFilters';

const t = ((k: string) => k) as unknown as TFunction;

const tmpl = (over: Partial<CollectionTemplate> = {}): CollectionTemplate => ({
  id: 'tpl-1',
  name: 'Standard interfaces',
  description: 'if_hc counters for every port',
  item_count: 6,
  ...over,
});

const prof = (over: Partial<ProfileSummary> = {}): ProfileSummary => ({
  id: 'p1',
  name: 'Cisco Catalyst switch',
  vendor: 'Cisco',
  category: 'l2-switch',
  poll_interval_secs: null,
  ...over,
});

const S_COLS = setColumns(t);
const S_DEFAULTS = defaultFilters(S_COLS);
const sf = (over: FilterState): FilterState => ({ ...S_DEFAULTS, ...over });
const hasSet = (r: CollectionTemplate, s: FilterState) => buildPredicate(S_COLS, s, 0)(r);

const CATEGORIES = [
  { token: 'l2-switch', label: 'L2 switch' },
  { token: 'router', label: 'Router' },
];
const P_COLS = [...profileColumns(t), ...profileCategoryColumns(t, CATEGORIES)];
const P_DEFAULTS = defaultFilters(P_COLS);
const pf = (over: FilterState): FilterState => ({ ...P_DEFAULTS, ...over });
const hasProf = (p: ProfileSummary, s: FilterState) => buildPredicate(P_COLS, s, 0)(p);

describe('the metric-sets filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(hasSet(tmpl(), S_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(S_COLS, S_DEFAULTS)).toBe(false);
  });

  it('asks the name and the description separately', () => {
    // The one box read both at once. The description is where the "why" lives, so "every set that
    // mentions counters" is a question about descriptions specifically.
    expect(hasSet(tmpl(), sf({ name: 'interfaces' }))).toBe(true);
    expect(hasSet(tmpl(), sf({ description: 'counters' }))).toBe(true);
    expect(hasSet(tmpl(), sf({ name: 'counters' }))).toBe(false);
  });

  it('survives a set with no description', () => {
    expect(hasSet(tmpl({ description: null }), sf({ name: 'Standard' }))).toBe(true);
    expect(hasSet(tmpl({ description: null }), sf({ description: 'x' }))).toBe(false);
  });

  it('labels every column', () => {
    const labels = setFilterLabels(t);
    for (const c of S_COLS) expect(labels[c.key]).toBeTruthy();
  });
});

describe('the device-profiles filter row', () => {
  it('shows everything when nothing is set', () => {
    expect(hasProf(prof(), P_DEFAULTS)).toBe(true);
    expect(isAnyFiltered(P_COLS, P_DEFAULTS)).toBe(false);
  });

  it('splits the poll interval into inherited and overridden, not into a number', () => {
    // ⚠️ The cell renders either a number of seconds or the word "default". Most rows have no
    // number at all, so a numeric range would answer a question about the minority and hide the
    // rest — the real question is which profiles override the system value.
    expect(PROFILE_INTERVAL).toEqual(['inherited', 'overridden']);
    expect(hasProf(prof(), pf({ interval: 'inherited' }))).toBe(true);
    expect(hasProf(prof(), pf({ interval: 'overridden' }))).toBe(false);
    expect(hasProf(prof({ poll_interval_secs: 30 }), pf({ interval: 'overridden' }))).toBe(true);
    // Zero is a real interval, not "unset" — the `== null` test is what keeps that true.
    expect(hasProf(prof({ poll_interval_secs: 0 }), pf({ interval: 'overridden' }))).toBe(true);
  });

  it('filters by the group heading, which is not a column', () => {
    // Category has no column — it is the heading rows are grouped under — so it is the one control
    // that lives in a `FilterBar`. It still has to work, because the search box it replaces matched
    // the category label.
    expect(hasProf(prof(), pf({ category: 'l2-switch' }))).toBe(true);
    expect(hasProf(prof(), pf({ category: 'router' }))).toBe(false);
    expect(hasProf(prof(), pf({ category: 'router,l2-switch' }))).toBe(true);
  });

  it('makes an unknown category selectable through the Other bucket', () => {
    // ⚠️ A profile carrying a token this build does not know is shown under "Other" by the
    // grouping. A filter that could not name that bucket would hide it with nothing saying so.
    const alien = prof({ category: 'quantum-relay' });
    expect(hasProf(alien, pf({ category: OTHER_CATEGORY }))).toBe(true);
    expect(hasProf(alien, pf({ category: 'l2-switch' }))).toBe(false);
    expect(hasProf(alien, P_DEFAULTS)).toBe(true);
  });

  it('asks the name and the vendor separately', () => {
    expect(hasProf(prof(), pf({ name: 'catalyst' }))).toBe(true);
    expect(hasProf(prof(), pf({ vendor: 'CISCO' }))).toBe(true);
    // "Cisco" is in both here; a vendor-less profile is where the two come apart.
    expect(hasProf(prof({ vendor: null }), pf({ vendor: 'cisco' }))).toBe(false);
    expect(hasProf(prof({ vendor: null }), pf({ name: 'catalyst' }))).toBe(true);
  });

  it('counts categories over the rows the other filters leave', () => {
    const rows = [prof(), prof({ id: 'p2', category: 'router', name: 'Juniper MX' })];
    // Selecting a category must not zero the others out — the rule that makes the control readable.
    const counts = facetCounts(rows, P_COLS, pf({ category: 'router' }), 'category', 0);
    expect(counts['l2-switch']).toBe(1);
    expect(counts.router).toBe(1);
    // …while a filter on a different column does narrow them.
    const narrowed = facetCounts(rows, P_COLS, pf({ name: 'juniper' }), 'category', 0);
    expect(narrowed['l2-switch']).toBe(0);
    expect(narrowed.router).toBe(1);
  });

  it('labels every column, including the one with no column', () => {
    const labels = { ...profileFilterLabels(t), category: 'Role' };
    for (const c of P_COLS) expect(labels[c.key as keyof typeof labels]).toBeTruthy();
  });
});
