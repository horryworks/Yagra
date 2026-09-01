// SPDX-License-Identifier: AGPL-3.0-only
// Same shape and same hazard as `eventRuleForm.test.ts`, with a sharper consequence: a
// classification rule decides which device profile a discovered node is bound to, so a field
// silently dropped on save re-classifies devices on the next sweep.
import { describe, expect, it } from 'vitest';
import { ruleToInput } from './classificationRuleForm';
import type { ClassificationRule, ClassificationRuleInput } from '../types/api';

/** One of every field, each distinguishable from a default. */
const INPUT: Required<ClassificationRuleInput> = {
  priority: 40,
  sysobjectid_prefix: '1.3.6.1.4.1.9.1',
  sysdescr_regex: 'Cisco IOS',
  profile_id: '33333333-3333-4333-8333-333333333333',
  vendor: 'Cisco',
  model: 'C9300',
  enabled: false,
} as Required<ClassificationRuleInput>;

const STORED = { id: 'c1', builtin: false, ...INPUT } as unknown as ClassificationRule;

describe('ruleToInput', () => {
  it('round-trips every field of a stored rule', () => {
    expect(ruleToInput(STORED)).toEqual(INPUT);
  });

  it('carries no key the input type does not have, and drops none that it does', () => {
    expect(Object.keys(ruleToInput(STORED)).sort()).toEqual(Object.keys(INPUT).sort());
  });

  it('preserves priority 0 and enabled false', () => {
    // Priority is ordered ascending, so 0 is the highest-precedence rule an operator can write —
    // exactly the one an "if truthy, copy" implementation would drop back to a default.
    const edge = ruleToInput({ ...STORED, priority: 0, enabled: false } as ClassificationRule);
    expect(edge.priority).toBe(0);
    expect(edge.enabled).toBe(false);
  });

  it('keeps a null matcher null rather than turning it into an empty string', () => {
    // A rule matching on `sysdescr_regex` alone leaves `sysobjectid_prefix` null. An empty string
    // is a *prefix that matches everything*, which would make that rule claim every device.
    const one = ruleToInput({ ...STORED, sysobjectid_prefix: null } as ClassificationRule);
    expect(one.sysobjectid_prefix).toBeNull();
  });
});
