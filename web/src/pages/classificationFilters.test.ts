// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for Nodes ▸ Classification rules' filter row (no DOM — Vitest node env).
//
// The screen this replaced had one search box over five fields, and the defect was not that it was
// coarse — it was that it conflated two opposite questions. `huawei` matched a rule that *points
// at* the Huawei profile and a rule that *matches* Huawei hardware identically, which are the two
// things you are trying to tell apart when a device classified wrong. The first describe below is
// that separation; everything else is the ordinary column behaviour.

import { describe, expect, it } from 'vitest';
import type { ClassificationRule } from '../types/api';
import { classificationRuleFilters } from './classificationFilters';
import { specColumns, type ColumnFilterSpec, type FilterState } from '../lib/columnFilter';
import { buildPredicate } from '../lib/filterPredicate';
import { encodeCondition } from '../lib/filterCondition';

const rule = (over: Partial<ClassificationRule> = {}): ClassificationRule => ({
  id: 'r1',
  priority: 10,
  enabled: true,
  profile_id: 'p-cisco',
  sysobjectid_prefix: '1.3.6.1.4.1.9',
  sysdescr_regex: null,
  vendor: null,
  model: null,
  ...over,
});

const t = ((k: string) => k) as unknown as Parameters<typeof classificationRuleFilters>[0];
/** The page's resolver: every rule here points at the Cisco profile unless a test says otherwise. */
const profileName = (id: string) => (id === 'p-huawei' ? 'Huawei USG' : 'Cisco IOS');
const term = (s: string) => encodeCondition({ term: s, mode: 'contains', not: false });

const specs = classificationRuleFilters(t, profileName);

function keeps(row: ClassificationRule, state: FilterState): boolean {
  return buildPredicate(specColumns(specs as Record<string, ColumnFilterSpec<ClassificationRule>>), state, 0)(row);
}

describe('"what it matches" and "what it points at" are different columns', () => {
  // A rule that MATCHES Huawei hardware but resolves to the Cisco profile.
  const matchesHuawei = rule({ vendor: 'Huawei', profile_id: 'p-cisco' });
  // A rule that POINTS AT the Huawei profile but matches on an OID prefix that says nothing.
  const pointsAtHuawei = rule({ vendor: null, profile_id: 'p-huawei' });

  it('finds the rule that matches Huawei hardware under `match`, and only that one', () => {
    expect(keeps(matchesHuawei, { match: term('huawei') })).toBe(true);
    expect(keeps(pointsAtHuawei, { match: term('huawei') })).toBe(false);
  });

  it('finds the rule that points at the Huawei profile under `profile`, and only that one', () => {
    expect(keeps(pointsAtHuawei, { profile: term('huawei') })).toBe(true);
    expect(keeps(matchesHuawei, { profile: term('huawei') })).toBe(false);
  });
});

describe('the match column', () => {
  it('shows everything when nothing is set', () => {
    expect(keeps(rule(), {})).toBe(true);
  });

  it('reads all four matcher fields', () => {
    expect(keeps(rule({ sysobjectid_prefix: '1.3.6.1.4.1.2011' }), { match: term('2011') })).toBe(
      true,
    );
    expect(keeps(rule({ sysdescr_regex: 'NE40E' }), { match: term('ne40') })).toBe(true);
    expect(keeps(rule({ vendor: 'Juniper' }), { match: term('junip') })).toBe(true);
    expect(keeps(rule({ model: 'MX204' }), { match: term('mx2') })).toBe(true);
  });

  it('does not let a term match across the gap between two fields', () => {
    // The fields are read as separate strings, not joined. Joining them would make `HuaweiNE40`
    // match a rule whose vendor is `Huawei` and whose regex is `NE40` — a match that exists only
    // in the concatenation and never in the data.
    const split = rule({ vendor: 'Huawei', model: 'NE40E' });
    expect(keeps(split, { match: term('Huawei') })).toBe(true);
    expect(keeps(split, { match: term('NE40') })).toBe(true);
    expect(keeps(split, { match: term('HuaweiNE40') })).toBe(false);
  });

  it('treats a null matcher as empty rather than as the string "null"', () => {
    expect(keeps(rule({ vendor: null }), { match: term('null') })).toBe(false);
  });

  it('accepts a regular expression when the mode says so', () => {
    const regex = encodeCondition({ term: '^1\\.3\\.6\\.1\\.4\\.1\\.9$', mode: 'regex', not: false });
    expect(keeps(rule({ sysobjectid_prefix: '1.3.6.1.4.1.9' }), { match: regex })).toBe(true);
    expect(keeps(rule({ sysobjectid_prefix: '1.3.6.1.4.1.99' }), { match: regex })).toBe(false);
  });
});

describe('the status column', () => {
  it('reads the boolean as the two tokens the options offer', () => {
    expect(keeps(rule({ enabled: true }), { status: 'enabled' })).toBe(true);
    expect(keeps(rule({ enabled: true }), { status: 'disabled' })).toBe(false);
    expect(keeps(rule({ enabled: false }), { status: 'disabled' })).toBe(true);
    expect(keeps(rule({ enabled: false }), { status: 'enabled' })).toBe(false);
  });

  it('offers exactly the two values it can read', () => {
    const status = specs.status;
    expect(status.kind).toBe('enum');
    if (status.kind !== 'enum') throw new Error('unreachable');
    expect(status.options.map((o) => o.value)).toEqual(['enabled', 'disabled']);
  });
});
