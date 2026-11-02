// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { LIVENESS_METRIC } from '../../lib/format';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';
import { nodeOverrideForm, nodeRuleRows, ownRule } from './nodeRules';

const NODE = '11111111-1111-1111-1111-111111111111';
const OTHER = '22222222-2222-2222-2222-222222222222';

function rule(over: Partial<StoredThreshold>): StoredThreshold {
  return {
    id: crypto.randomUUID(),
    scope_level: 'profile',
    scope_ids: ['p1'],
    metric: 'cpu_util',
    direction: 'above',
    warning: 80,
    critical: 90,
    warning_below: null,
    critical_below: null,
    warning_above: 80,
    critical_above: 90,
    dwell_samples: 3,
    ...over,
  } as StoredThreshold;
}

const m = (r: StoredThreshold, inForce: boolean): MatchingThreshold => ({ rule: r, in_force: inForce });

describe('nodeRuleRows', () => {
  it('groups by metric, keeps the server order inside a group and puts liveness first', () => {
    const profile = rule({});
    const folder = rule({ scope_level: 'group_id', scope_ids: ['f1'], warning_above: 70 });
    const live = rule({ scope_level: 'global', scope_ids: [], metric: LIVENESS_METRIC });
    const mem = rule({ metric: 'mem_util' });
    const rows = nodeRuleRows([m(folder, true), m(profile, false), m(mem, true), m(live, true)]);
    expect(rows.map((r) => r.metric)).toEqual([LIVENESS_METRIC, 'cpu_util', 'mem_util']);
    expect(rows[1].inForce).toEqual([folder]);
    expect(rows[1].overridden).toEqual([profile]);
  });

  it('answers an empty list with no rows', () => {
    expect(nodeRuleRows([])).toEqual([]);
  });
});

describe('ownRule', () => {
  it('is the in-force node rule that names only this node', () => {
    const own = rule({ scope_level: 'node', scope_ids: [NODE] });
    const [row] = nodeRuleRows([m(own, true), m(rule({}), false)]);
    expect(ownRule(row, NODE)).toBe(own);
  });

  it('is nothing for an inherited rule or one shared with other nodes', () => {
    const [inherited] = nodeRuleRows([m(rule({}), true)]);
    expect(ownRule(inherited, NODE)).toBeNull();
    const [shared] = nodeRuleRows([m(rule({ scope_level: 'node', scope_ids: [NODE, OTHER] }), true)]);
    expect(ownRule(shared, NODE)).toBeNull();
  });
});

describe('nodeOverrideForm', () => {
  it('starts a node-level rule for this node from the bounds in force', () => {
    const [row] = nodeRuleRows([m(rule({ warning_above: 70, critical_above: 85 }), true)]);
    const f = nodeOverrideForm(row, NODE);
    expect(f.level).toBe('node');
    expect(f.scopeIds).toEqual([NODE]);
    expect(f.metric).toBe('cpu_util');
    expect(f.warningAbove).toBe('70');
    expect(f.criticalAbove).toBe('85');
  });
});
