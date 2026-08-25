// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { StoredThreshold } from '../types/api';
import { collectedMetrics, profileRuleGap, ruleIsBaselineFor } from './profileRuleGap';

const P = '11111111-1111-1111-1111-111111111111';
const OTHER = '22222222-2222-2222-2222-222222222222';

/** A stored rule with only the fields this module reads set to anything meaningful. */
function rule(
  metric: string,
  scope_level: StoredThreshold['scope_level'],
  scope_ids: string[] = [],
): StoredThreshold {
  return {
    id: '00000000-0000-0000-0000-000000000000',
    metric,
    dwell_samples: 3,
    direction: 'above',
    scope_level,
    scope_ids,
  };
}

describe('ruleIsBaselineFor', () => {
  // The acceptance half first: with only the rejection cases below, a function that answered
  // `false` to everything would pass every one of them and report the whole fleet as uncovered
  // (`rejection-only-tests-pass-when-everything-rejects`).
  it('accepts a fleet-wide rule and a profile rule that names this profile', () => {
    expect(ruleIsBaselineFor(rule('m', 'global'), P)).toBe(true);
    expect(ruleIsBaselineFor(rule('m', 'profile', [OTHER, P]), P)).toBe(true);
  });

  it('refuses a profile rule that names a different profile', () => {
    expect(ruleIsBaselineFor(rule('m', 'profile', [OTHER]), P)).toBe(false);
    expect(ruleIsBaselineFor(rule('m', 'profile', []), P)).toBe(false);
  });

  // ADR-106 決定 2. These reach *some* of a profile's nodes, so at the profile dimension there is no
  // answer; and `resolve_effective` takes only the most specific level present, so they replace a
  // profile rule rather than adding to one. Counting them as a baseline is what would make the
  // panel say "covered" about a metric that fires on four nodes out of forty.
  it('refuses every scope narrower than profile, even when it names this profile id', () => {
    for (const level of ['group', 'group_id', 'node', 'interface'] as const) {
      expect(ruleIsBaselineFor(rule('m', level, [P]), P)).toBe(false);
    }
  });
});

describe('collectedMetrics', () => {
  it('folds the loaded sets into one distinct, sorted list', () => {
    const loaded = new Map([
      ['a', ['cisco_cpu_5min', 'cisco_env_temp']],
      ['b', ['cisco_cpu_5min', 'ucd_swap_used_pct']],
    ]);
    expect(collectedMetrics(['a', 'b'], loaded, new Set())).toEqual({
      state: 'ready',
      metrics: ['cisco_cpu_5min', 'cisco_env_temp', 'ucd_swap_used_pct'],
    });
  });

  it('is ready with nothing attached', () => {
    expect(collectedMetrics([], new Map(), new Set())).toEqual({ state: 'ready', metrics: [] });
  });

  // The two not-an-answer states are kept apart because they render differently: one resolves
  // itself and says nothing meanwhile, the other is a standing admission.
  it('is loading while a set is still in flight, and failed once one gives up', () => {
    const loaded = new Map([['a', ['m']]]);
    expect(collectedMetrics(['a', 'b'], loaded, new Set())).toEqual({ state: 'loading' });
    expect(collectedMetrics(['a', 'b'], loaded, new Set(['b']))).toEqual({ state: 'failed' });
  });

  // The direction that matters: a set that failed must not read as a set with no metrics, which
  // would make its profile look *more* covered than it is.
  it('a failed set outranks the ones that loaded', () => {
    const loaded = new Map([
      ['a', ['m']],
      ['b', []],
    ]);
    expect(collectedMetrics(['a', 'b'], loaded, new Set(['b']))).toEqual({ state: 'failed' });
  });
});

describe('profileRuleGap', () => {
  const base = { rulesTruncated: false, profileId: P };

  it('reports covered when every metric has a baseline, from either level', () => {
    expect(
      profileRuleGap({
        ...base,
        metrics: ['icmp_rtt_ms', 'cisco_cpu_5min'],
        rules: [rule('icmp_rtt_ms', 'global'), rule('cisco_cpu_5min', 'profile', [P])],
      }),
    ).toEqual({ kind: 'covered', total: 2 });
  });

  it('names the metrics with no baseline, keeping the order it was given', () => {
    expect(
      profileRuleGap({
        ...base,
        metrics: ['cisco_cpu_5min', 'cisco_env_temp', 'ucd_swap_used_pct'],
        rules: [rule('cisco_env_temp', 'profile', [P])],
      }),
    ).toEqual({ kind: 'gaps', missing: ['cisco_cpu_5min', 'ucd_swap_used_pct'], total: 3 });
  });

  it('a rule for the metric at a narrower scope does not close the gap', () => {
    expect(
      profileRuleGap({ ...base, metrics: ['cisco_env_temp'], rules: [rule('cisco_env_temp', 'node', [P])] }),
    ).toEqual({ kind: 'gaps', missing: ['cisco_env_temp'], total: 1 });
  });

  // 🚨 The cap. A rule past `THRESHOLDS_MAX` is a rule this cannot see, so a gap computed from a
  // prefix is a guess — and it is a guess in the loud direction, naming metrics that are covered.
  it('refuses to answer at all when the rule list was truncated', () => {
    expect(
      profileRuleGap({ ...base, rulesTruncated: true, metrics: ['cisco_env_temp'], rules: [] }),
    ).toEqual({ kind: 'unchecked' });
  });

  it('refuses to answer when the rules never loaded', () => {
    expect(profileRuleGap({ ...base, metrics: ['cisco_env_temp'], rules: null })).toEqual({
      kind: 'unchecked',
    });
  });

  // Separate from `covered` with a total of zero: a profile with no sets attached should say
  // nothing, not "0 metrics, all covered".
  it('is empty when the profile collects nothing', () => {
    expect(profileRuleGap({ ...base, metrics: [], rules: [] })).toEqual({ kind: 'empty' });
  });
});
