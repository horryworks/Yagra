// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { overriddenRows } from './thresholdOverrides';

const RULE_A = '00000000-0000-4000-8000-0000000000a1';
const RULE_B = '00000000-0000-4000-8000-0000000000b2';

describe('overriddenRows', () => {
  it('reads the node count the server gave each rule', () => {
    const out = overriddenRows({ overridden: { [RULE_A]: 3, [RULE_B]: 1 } });
    expect(out.get(RULE_A)).toBe(3);
    expect(out.get(RULE_B)).toBe(1);
    expect(out.size).toBe(2);
  });

  it('marks nothing for a zero, an empty map, or an answer without the field', () => {
    expect(overriddenRows({ overridden: { [RULE_A]: 0 } }).size).toBe(0);
    expect(overriddenRows({ overridden: {} }).size).toBe(0);
    expect(overriddenRows({}).size).toBe(0);
  });
});
