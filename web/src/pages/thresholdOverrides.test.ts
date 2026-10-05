// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { StoredThreshold } from '../types/api';
import { overriddenRows } from './thresholdOverrides';

const NODE_A = '00000000-0000-4000-8000-0000000000a1';
const NODE_B = '00000000-0000-4000-8000-0000000000b2';

let seq = 0;
function rule(over: Partial<StoredThreshold>): StoredThreshold {
  seq += 1;
  return {
    id: `r${seq}`,
    scope_level: 'global',
    scope_ids: [],
    metric: 'icmp_rtt_ms',
    row_match: null,
    direction: 'above',
    warning: 150,
    critical: 400,
    warning_below: null,
    critical_below: null,
    warning_above: 150,
    critical_above: 400,
    dwell_samples: 3,
    ...over,
  } as StoredThreshold;
}

describe('overriddenRows', () => {
  it('marks a fleet-wide rule with the nodes a node rule takes over', () => {
    const global = rule({});
    const node = rule({ scope_level: 'node', scope_ids: [NODE_A, NODE_B] });
    const out = overriddenRows([global, node]);
    expect(out.get(global.id)).toEqual({ kind: 'nodes', count: 2 });
    // The narrower rule itself is not overridden by anything.
    expect(out.has(node.id)).toBe(false);
  });

  it('counts a node once when a node rule and a port rule both name it', () => {
    const global = rule({});
    const node = rule({ scope_level: 'node', scope_ids: [NODE_A] });
    const port = rule({ scope_level: 'interface', scope_ids: [`${NODE_A}:3`] });
    expect(overriddenRows([global, node, port]).get(global.id)).toEqual({
      kind: 'nodes',
      count: 1,
    });
  });

  it('counts rules, not nodes, when a profile or folder rule is among the overriders', () => {
    const global = rule({});
    const profile = rule({ scope_level: 'profile', scope_ids: ['p1'] });
    const node = rule({ scope_level: 'node', scope_ids: [NODE_A] });
    expect(overriddenRows([global, profile, node]).get(global.id)).toEqual({
      kind: 'rules',
      count: 2,
    });
  });

  it('ignores a narrower rule on another metric', () => {
    const global = rule({});
    const other = rule({ scope_level: 'node', scope_ids: [NODE_A], metric: 'cpu_pct' });
    expect(overriddenRows([global, other]).size).toBe(0);
  });

  it('marks a node rule only with the ports on its own nodes', () => {
    const node = rule({ scope_level: 'node', scope_ids: [NODE_A] });
    const mine = rule({ scope_level: 'interface', scope_ids: [`${NODE_A}:1`] });
    const theirs = rule({ scope_level: 'interface', scope_ids: [`${NODE_B}:1`] });
    expect(overriddenRows([node, mine, theirs]).get(node.id)).toEqual({ kind: 'nodes', count: 1 });
    expect(overriddenRows([node, theirs]).has(node.id)).toBe(false);
  });

  it('does not claim an override the list cannot prove', () => {
    // Whether a node is in the profile is not in the rule list, so this pair is left unmarked.
    const profile = rule({ scope_level: 'profile', scope_ids: ['p1'] });
    const node = rule({ scope_level: 'node', scope_ids: [NODE_A] });
    expect(overriddenRows([profile, node]).size).toBe(0);
    // Two fleet-wide rules on one metric are a tie, not an override.
    expect(overriddenRows([rule({}), rule({})]).size).toBe(0);
  });

  it('treats different row patterns as reaching different rows', () => {
    const global = rule({ row_match: 'I/O' });
    const other = rule({ scope_level: 'node', scope_ids: [NODE_A], row_match: 'Processor' });
    const same = rule({ scope_level: 'node', scope_ids: [NODE_B], row_match: 'i/o' });
    const blank = rule({ scope_level: 'node', scope_ids: [NODE_A] });
    expect(overriddenRows([global, other]).size).toBe(0);
    expect(overriddenRows([global, same]).get(global.id)).toEqual({ kind: 'nodes', count: 1 });
    expect(overriddenRows([global, blank]).get(global.id)).toEqual({ kind: 'nodes', count: 1 });
  });
});
