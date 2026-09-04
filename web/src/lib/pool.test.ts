// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NodeAssignment, NodeGroup, PolledBy, PoolOption } from '../types/api';
import i18n from '../i18n';
import {
  MAX_POOL_LEN,
  POOL_CHIP_LIMIT,
  inheritedGroupPool,
  isValidPoolName,
  poolChoices,
  poolFactLabel,
  polledByIsWarning,
  polledByLabel,
} from './pool';

// English is bundled synchronously (see i18n.ts), so `t` resolves the `nodes:` keys below.
const t = i18n.t.bind(i18n);

const group = (over: Partial<NodeGroup> = {}): NodeGroup => ({
  id: 'g1',
  name: 'Tokyo',
  group_type: 'site',
  prefixes: [],
  parent_id: null,
  sort_order: 0,
  latitude: null,
  longitude: null,
  geo_source: 'unset',
  pool: null,
  ...over,
});

const assignment = (over: Partial<NodeAssignment> = {}): NodeAssignment => ({
  pool: 'tokyo',
  pool_source: 'node',
  pool_source_group_id: null,
  polled_by: { state: 'assigned', poller_id: 'edge-1' },
  ...over,
});

describe('isValidPoolName', () => {
  it('accepts the server alphabet and treats empty as "inherit"', () => {
    expect(isValidPoolName('tokyo')).toBe(true);
    expect(isValidPoolName('edge-1_lab')).toBe(true);
    expect(isValidPoolName('  tokyo  ')).toBe(true);
    // Blank is valid: the forms send '' to clear an assignment back to inherited.
    expect(isValidPoolName('')).toBe(true);
    expect(isValidPoolName('   ')).toBe(true);
  });

  it('rejects anything that is not a single NATS subject token', () => {
    // A dot would publish to `yagra.jobs.tokyo.1`, a subject no poller subscribes to.
    expect(isValidPoolName('tokyo.1')).toBe(false);
    expect(isValidPoolName('east dc')).toBe(false);
    expect(isValidPoolName('a/b')).toBe(false);
    expect(isValidPoolName('*')).toBe(false);
  });

  it('enforces the same length bound as the server', () => {
    expect(isValidPoolName('p'.repeat(MAX_POOL_LEN))).toBe(true);
    expect(isValidPoolName('p'.repeat(MAX_POOL_LEN + 1))).toBe(false);
  });
});

describe('polledByLabel', () => {
  const state = (s: PolledBy['state'], poller_id: string | null = null): PolledBy => ({
    state: s,
    poller_id,
  });

  it('names the poller only when the node is actually assigned', () => {
    expect(polledByLabel(state('assigned', 'edge-1'), t)).toBe('edge-1');
    // Every other state is a distinct condition, never a poller name.
    expect(polledByLabel(state('legacy_fanout'), t)).toBe('No live poller');
    expect(polledByLabel(state('pending'), t)).toBe('Not yet assigned');
    expect(polledByLabel(state('meraki'), t)).toBe('Meraki collector');
    expect(polledByLabel(state('unknown'), t)).toBe('Unknown');
  });

  it('renders an em dash when the assignment could not be loaded', () => {
    expect(polledByLabel(undefined, t)).toBe('—');
    // An `assigned` state with no id shouldn't happen, but must not render "null".
    expect(polledByLabel(state('assigned'), t)).toBe('—');
  });
});

describe('polledByIsWarning', () => {
  it('flags only the pool that has no live poller', () => {
    // legacy_fanout means jobs go to a subject with no subscriber — probably unmonitored.
    expect(polledByIsWarning({ state: 'legacy_fanout', poller_id: null })).toBe(true);
    expect(polledByIsWarning({ state: 'assigned', poller_id: 'edge-1' })).toBe(false);
    expect(polledByIsWarning({ state: 'pending', poller_id: null })).toBe(false);
    expect(polledByIsWarning({ state: 'meraki', poller_id: null })).toBe(false);
    expect(polledByIsWarning(undefined)).toBe(false);
  });
});

describe('poolFactLabel', () => {
  const names = (id: string) => (id === 'g1' ? 'Tokyo' : undefined);

  it('shows a node-set pool plainly', () => {
    expect(poolFactLabel(assignment(), names, t)).toBe('tokyo');
  });

  it('annotates an inherited pool with the folder it came from', () => {
    const inherited = assignment({ pool_source: 'group', pool_source_group_id: 'g1' });
    expect(poolFactLabel(inherited, names, t)).toBe('tokyo (from Tokyo)');
    // An unresolvable folder degrades to the bare pool rather than "from undefined".
    const orphan = assignment({ pool_source: 'group', pool_source_group_id: 'gone' });
    expect(poolFactLabel(orphan, names, t)).toBe('tokyo');
  });

  it('marks the implicit default', () => {
    const fallback = assignment({ pool: 'default', pool_source: 'default' });
    expect(poolFactLabel(fallback, names, t)).toBe('default (default)');
  });

  it('renders an em dash with no assignment', () => {
    expect(poolFactLabel(undefined, names, t)).toBe('—');
  });
});

describe('poolChoices', () => {
  const opt = (name: string, live = true): PoolOption => ({ name, live });

  it('keeps the server order and marks liveness', () => {
    const chips = poolChoices([opt('default'), opt('tokyo'), opt('osaka', false)], null);
    expect(chips.map((c) => c.name)).toEqual(['default', 'tokyo', 'osaka']);
    expect(chips.map((c) => c.live)).toEqual([true, true, false]);
    // Nothing is current when the target inherits.
    expect(chips.some((c) => c.current)).toBe(false);
  });

  it('puts the current pool first and marks it, without duplicating it', () => {
    const chips = poolChoices([opt('default'), opt('tokyo'), opt('osaka')], 'tokyo');
    expect(chips.map((c) => c.name)).toEqual(['tokyo', 'default', 'osaka']);
    expect(chips[0].current).toBe(true);
    expect(chips.filter((c) => c.name === 'tokyo')).toHaveLength(1);
  });

  it('still shows a current pool the server no longer lists', () => {
    // e.g. its last poller went away and nothing else references it — the menu must not silently
    // fail to show what the node is actually set to.
    const chips = poolChoices([opt('default')], 'retired');
    expect(chips[0]).toEqual({ name: 'retired', live: false, current: true });
    expect(chips.map((c) => c.name)).toContain('default');
  });

  it('treats blank/whitespace as inherited', () => {
    for (const current of [null, undefined, '', '   ']) {
      expect(poolChoices([opt('default')], current).some((c) => c.current)).toBe(false);
    }
  });

  it('caps the row but never drops the current pool', () => {
    const many = Array.from({ length: 20 }, (_, i) => opt(`p${i}`));
    expect(poolChoices(many, null)).toHaveLength(POOL_CHIP_LIMIT);
    const withCurrent = poolChoices(many, 'p19');
    expect(withCurrent).toHaveLength(POOL_CHIP_LIMIT);
    expect(withCurrent[0]).toMatchObject({ name: 'p19', current: true });
  });
});

describe('inheritedGroupPool', () => {
  it('returns the nearest ancestor that sets a pool', () => {
    const groups = [
      group({ id: 'root', pool: 'tokyo' }),
      group({ id: 'mid', parent_id: 'root' }),
      group({ id: 'leaf', parent_id: 'mid', pool: 'edge' }),
    ];
    expect(inheritedGroupPool(groups, 'mid')).toBe('tokyo');
    // The folder's OWN pool is ignored — this previews what it would inherit.
    expect(inheritedGroupPool(groups, 'leaf')).toBe('edge');
    expect(inheritedGroupPool(groups, 'root')).toBe('tokyo');
  });

  it('returns undefined when nothing is inherited', () => {
    const groups = [group({ id: 'root' }), group({ id: 'mid', parent_id: 'root' })];
    expect(inheritedGroupPool(groups, 'mid')).toBeUndefined();
    expect(inheritedGroupPool(groups, null)).toBeUndefined();
    expect(inheritedGroupPool(groups, undefined)).toBeUndefined();
    // A parent that no longer exists.
    expect(inheritedGroupPool(groups, 'gone')).toBeUndefined();
  });

  it('treats a blank stored pool as unset', () => {
    const groups = [group({ id: 'root', pool: 'tokyo' }), group({ id: 'mid', parent_id: 'root', pool: '  ' })];
    expect(inheritedGroupPool(groups, 'mid')).toBe('tokyo');
  });

  it('does not hang on cyclic ancestry', () => {
    // There is no DB constraint against a cycle, so the walk must be bounded.
    const groups = [
      group({ id: 'a', parent_id: 'b' }),
      group({ id: 'b', parent_id: 'a' }),
    ];
    expect(inheritedGroupPool(groups, 'a')).toBeUndefined();
  });
});
