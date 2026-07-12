import { describe, expect, it } from 'vitest';
import {
  buildNodeTree,
  descendantNodes,
  flattenTree,
  flatRowKey,
  groupOptions,
  groupPath,
  isSelfOrDescendant,
  tallyStates,
} from './nodeTree';
import type { NodeGroup, NodeState, NodeSummary } from '../types/api';

const group = (
  id: string,
  name: string,
  parent: string | null = null,
  sort_order = 0,
): NodeGroup => ({
  id,
  name,
  group_type: 'generic',
  parent_id: parent,
  sort_order,
  latitude: null,
  longitude: null,
});

const node = (
  id: string,
  name: string,
  groupId: string | null,
  sort_order = 0,
  state: NodeState = 'ok',
): NodeSummary => ({
  id,
  name,
  address: '10.0.0.1',
  state,
  vendor: null,
  model: null,
  group_id: groupId,
  sort_order,
});

describe('flattenTree', () => {
  const tree = () =>
    buildNodeTree(
      [group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1')],
      [node('n1', 'sw1', 'g2'), node('n2', 'router', null)],
    );

  it('emits rows in display order: group, nested group, node, then the ungrouped section', () => {
    const rows = flattenTree(tree(), { collapsed: {}, filter: '' });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'n:n1', 'ungrouped-head', 'n:n2']);
    const g1 = rows[0];
    expect(g1.kind === 'group' && g1.depth).toBe(0);
    const g2 = rows[1];
    expect(g2.kind === 'group' && g2.depth).toBe(1);
    // The group carries its rolled-up subtree health (here: the one descendant node) for the bar.
    expect(g1.kind === 'group' && g1.tally.total).toBe(1);
  });

  it('collapsing a group hides its descendants but keeps the group row', () => {
    const rows = flattenTree(tree(), { collapsed: { g1: true }, filter: '' });
    // g1 present (collapsed), g2/n1 hidden, ungrouped still shows.
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'ungrouped-head', 'n:n2']);
    const g1 = rows[0];
    expect(g1.kind === 'group' && g1.isOpen).toBe(false);
  });

  it('filtering force-expands and hides non-matching rows', () => {
    const rows = flattenTree(tree(), { collapsed: { g1: true }, filter: 'sw1' });
    // The match reveals the ancestor groups (force-expanded) down to the node; router is filtered out.
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'n:n1']);
  });

  it('a completely empty inventory yields no rows (page shows its own empty state)', () => {
    const empty = buildNodeTree([], []);
    expect(flattenTree(empty, { collapsed: {}, filter: '' })).toEqual([]);
  });

  it('shows the ungrouped header alongside groups even when there are no ungrouped nodes', () => {
    const t = buildNodeTree([group('g1', 'Tokyo')], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, { collapsed: {}, filter: '' });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1', 'ungrouped-head']);
  });
});

describe('flattenTree lazy load (A-3)', () => {
  const counts = (partial: Partial<Record<NodeState, number>>): Record<NodeState, number> => ({
    ok: 0,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
    ...partial,
  });

  it('rolls a group row up from server counts and rolls sub-group counts into the parent', () => {
    // Tokyo (g1) has a sub-group Rack A (g2); no members are loaded, only per-group direct counts.
    const t = buildNodeTree([group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: { g1: counts({ ok: 2 }), g2: counts({ critical: 1 }) },
      loadedGroups: new Set(),
    });
    const g1 = rows.find((r) => flatRowKey(r) === 'g:g1');
    // Tokyo's subtree tally = its own 2 ok + Rack A's 1 critical.
    expect(g1?.kind === 'group' && g1.tally.total).toBe(3);
    expect(g1?.kind === 'group' && g1.tally.counts.critical).toBe(1);
    expect(g1?.kind === 'group' && g1.hasChildren).toBe(true);
  });

  it('emits a loading placeholder for an open group whose members are not loaded', () => {
    const t = buildNodeTree([group('g1', 'Tokyo')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: { g1: counts({ ok: 5 }) },
      loadedGroups: new Set(), // g1 not loaded
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'loading:g1', 'ungrouped-head']);
  });

  it('emits member rows once the group is loaded, and no loading row for an empty group', () => {
    // Explicit sort_order pins the sibling order (g1 before g2) independent of name.
    const t = buildNodeTree(
      [group('g1', 'Tokyo', null, 1), group('g2', 'Empty', null, 2)],
      [node('n1', 'sw1', 'g1')],
    );
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: { g1: counts({ ok: 1 }), g2: counts({}) }, // g2 has no direct members
      loadedGroups: new Set(['g1']), // g1 loaded, g2 not (but empty → no loading row)
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1', 'g:g2', 'ungrouped-head']);
  });
});

describe('buildNodeTree', () => {
  it('nests groups and places nodes under their group', () => {
    const groups = [group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1')];
    const nodes = [node('n1', 'sw1', 'g2'), node('n2', 'router', null)];
    const tree = buildNodeTree(groups, nodes);

    expect(tree.roots).toHaveLength(1);
    expect(tree.roots[0].id).toBe('g1');
    expect(tree.roots[0].children[0].id).toBe('g2');
    expect(tree.roots[0].children[0].nodes.map((n) => n.id)).toEqual(['n1']);
    expect(tree.ungrouped.map((n) => n.id)).toEqual(['n2']);
  });

  it('treats an unknown parent/group reference as top-level / ungrouped', () => {
    const groups = [group('g1', 'Orphan', 'missing')];
    const nodes = [node('n1', 'x', 'gone')];
    const tree = buildNodeTree(groups, nodes);
    expect(tree.roots.map((g) => g.id)).toEqual(['g1']);
    expect(tree.ungrouped.map((n) => n.id)).toEqual(['n1']);
  });

  it('falls back to name order when sort_order is equal/unset', () => {
    const groups = [group('a', 'Zeta'), group('b', 'Alpha')];
    const nodes = [node('n2', 'zzz', null), node('n1', 'aaa', null)];
    const tree = buildNodeTree(groups, nodes);
    expect(tree.roots.map((g) => g.name)).toEqual(['Alpha', 'Zeta']);
    expect(tree.ungrouped.map((n) => n.name)).toEqual(['aaa', 'zzz']);
  });

  it('orders siblings by sort_order ahead of name (manual drag order wins)', () => {
    // Alphabetically Alpha < Zeta, but the manual order puts Zeta first.
    const groups = [group('a', 'Zeta', null, 1), group('b', 'Alpha', null, 2)];
    const nodes = [node('n1', 'aaa', null, 2), node('n2', 'zzz', null, 1)];
    const tree = buildNodeTree(groups, nodes);
    expect(tree.roots.map((g) => g.name)).toEqual(['Zeta', 'Alpha']);
    expect(tree.ungrouped.map((n) => n.name)).toEqual(['zzz', 'aaa']);
  });
});

describe('descendantNodes', () => {
  it('gathers a group’s own nodes plus those of every descendant group', () => {
    const groups = [
      group('tok', 'Tokyo'),
      group('core', 'Core', 'tok'),
      group('dist', 'Distribution', 'tok'),
    ];
    const nodes = [
      node('n1', 'core-1', 'core'),
      node('n2', 'core-2', 'core'),
      node('n3', 'dist-1', 'dist'),
      node('n4', 'direct', 'tok'), // a node directly on the parent group
      node('n5', 'elsewhere', null),
    ];
    const tree = buildNodeTree(groups, nodes);
    const tokyo = tree.roots[0];
    expect(descendantNodes(tokyo).map((n) => n.id).sort()).toEqual(['n1', 'n2', 'n3', 'n4']);
    const core = tokyo.children.find((c) => c.id === 'core')!;
    expect(descendantNodes(core).map((n) => n.id)).toEqual(['n1', 'n2']);
  });
});

describe('tallyStates', () => {
  it('counts every state and totals the problem (need-attention) states', () => {
    const nodes = [
      node('a', 'a', null, 0, 'ok'),
      node('b', 'b', null, 0, 'ok'),
      node('c', 'c', null, 0, 'warning'),
      node('d', 'd', null, 0, 'critical'),
      node('e', 'e', null, 0, 'unreachable'),
      node('f', 'f', null, 0, 'maintenance'),
      node('g', 'g', null, 0, 'unknown'),
    ];
    const t = tallyStates(nodes);
    expect(t.total).toBe(7);
    expect(t.counts.ok).toBe(2);
    expect(t.counts.warning).toBe(1);
    expect(t.counts.critical).toBe(1);
    expect(t.counts.unreachable).toBe(1);
    expect(t.counts.maintenance).toBe(1);
    expect(t.counts.unknown).toBe(1);
    // warning + critical + unreachable — maintenance/unknown are not "problems".
    expect(t.needAttention).toBe(3);
  });

  it('is all-zero for an empty set', () => {
    const t = tallyStates([]);
    expect(t.total).toBe(0);
    expect(t.needAttention).toBe(0);
    expect(t.counts.ok).toBe(0);
  });
});

describe('groupPath', () => {
  const groups = [group('a', 'Tokyo'), group('b', 'Edge', 'a'), group('c', 'Firewall', 'b')];

  it('returns the ancestor chain from the root down to the group', () => {
    expect(groupPath(groups, 'c')).toEqual(['Tokyo', 'Edge', 'Firewall']);
    expect(groupPath(groups, 'a')).toEqual(['Tokyo']);
  });

  it('returns an empty path for a null or unknown id', () => {
    expect(groupPath(groups, null)).toEqual([]);
    expect(groupPath(groups, 'missing')).toEqual([]);
  });
});

describe('groupOptions', () => {
  it('flattens the hierarchy into depth-indented, name-sorted options', () => {
    const groups = [
      group('a', 'Tokyo'),
      group('b', 'Osaka'),
      group('a1', 'Edge', 'a'),
      group('a2', 'Core', 'a'),
    ];
    const opts = groupOptions(groups);
    // Top-level groups name-sorted, each parent's children following it, indented one level
    // (the indent uses non-breaking spaces so HTML <option> leading space isn't collapsed).
    expect(opts.map((o) => o.label.trim())).toEqual(['Osaka', 'Tokyo', 'Core', 'Edge']);
    const indent = (s: string) => s.length - s.trimStart().length;
    expect(opts.map((o) => indent(o.label))).toEqual([0, 0, 2, 2]);
  });
});

describe('isSelfOrDescendant', () => {
  const groups = [group('a', 'A'), group('b', 'B', 'a'), group('c', 'C', 'b')];

  it('flags self and descendants (cycle guard for moves)', () => {
    expect(isSelfOrDescendant(groups, 'a', 'a')).toBe(true); // self
    expect(isSelfOrDescendant(groups, 'a', 'c')).toBe(true); // c is under a
    expect(isSelfOrDescendant(groups, 'b', 'c')).toBe(true); // c is under b
  });

  it('allows moves that do not nest a group inside its own subtree', () => {
    expect(isSelfOrDescendant(groups, 'c', 'a')).toBe(false); // a is not under c
    expect(isSelfOrDescendant(groups, 'b', 'a')).toBe(false);
  });
});
