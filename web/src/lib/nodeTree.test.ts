// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  UNGROUPED,
  asGroupType,
  buildNodeTree,
  descendantNodes,
  filterTerm,
  findTreeGroup,
  flatRowKey,
  flattenTree,
  groupDeletionImpact,
  groupOptions,
  groupPath,
  isSelfOrDescendant,
  mergeNodesById,
  revealedGroupKeys,
  subtreeGroupIds,
  tallyStates,
  type StateCounts,
  type TreeGroup,
  visibleOpenGroupKeys,
} from './nodeTree';
import type { TFunction } from 'i18next';
import { GROUP_TYPES } from '../types/api';
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
  prefixes: [],
  parent_id: parent,
  sort_order,
  latitude: null,
  longitude: null,
  geo_source: 'unset',
  pool: null,
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
  kind: 'device',
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

describe('flattenTree — narrowed by a filter the tree cannot see (ADR-053 Inc.6)', () => {
  // The state / kind / pool controls run server-side, so the caller hands in the rows that survived
  // and sets `narrowed`. Everything the tree does about "am I filtering" hangs off that flag, and
  // before it existed every one of those tests was `is the search term non-empty` — so picking
  // *Critical* with an empty box hid nothing at all.
  const wide = () =>
    buildNodeTree(
      [
        group('g1', 'Japan'),
        group('g1a', 'Matsuyama', 'g1'),
        group('g2', 'Internet Sites'),
        group('g2a', 'DNS', 'g2'),
      ],
      [node('n1', 'fw01', 'g1a'), node('n2', 'test.example', 'g2a', 0, 'critical')],
    );
  /** What the page hands in once a state filter has been applied: only the surviving nodes. */
  const narrowedTree = () =>
    buildNodeTree(
      [
        group('g1', 'Japan'),
        group('g1a', 'Matsuyama', 'g1'),
        group('g2', 'Internet Sites'),
        group('g2a', 'DNS', 'g2'),
      ],
      [node('n2', 'test.example', 'g2a', 0, 'critical')],
    );

  it('drops the folders with nothing left under them', () => {
    // The reported bug: Japan / Matsuyama have no critical node, and they stayed on screen.
    const rows = flattenTree(narrowedTree(), { collapsed: {}, filter: '', narrowed: true });
    expect(rows.map(flatRowKey)).toEqual(['g:g2', 'g:g2a', 'n:n2']);
  });

  it('keeps every folder when it is NOT told it is narrowing', () => {
    // The same tree without the flag — which is what shipped, and why nothing was hidden.
    const rows = flattenTree(narrowedTree(), { collapsed: {}, filter: '' });
    expect(rows.map(flatRowKey)).toContain('g:g1');
    expect(rows.map(flatRowKey)).toContain('g:g1a');
  });

  it('force-expands, so a collapsed folder cannot hide its own match', () => {
    const rows = flattenTree(narrowedTree(), {
      collapsed: { g2: true, g2a: true },
      filter: '',
      narrowed: true,
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g2', 'g:g2a', 'n:n2']);
  });

  it('does not match a folder by name — a state filter says nothing about names', () => {
    // ⚠️ The one thing that must stay tied to the term. If `narrowed` also enabled name matching,
    // an empty term would match every folder (`''.includes('')` is true) and nothing would hide.
    const rows = flattenTree(narrowedTree(), { collapsed: {}, filter: '', narrowed: true });
    expect(rows.map(flatRowKey)).not.toContain('g:g1');
  });

  it('keeps every surviving node, since the rejecting was already done', () => {
    // Two nodes in one folder, both handed in: the term is empty, so neither may be dropped here.
    const t = buildNodeTree(
      [group('g2', 'Internet Sites'), group('g2a', 'DNS', 'g2')],
      [node('a', 'alpha', 'g2a'), node('b', 'beta', 'g2a', 1)],
    );
    const rows = flattenTree(t, { collapsed: {}, filter: '', narrowed: true });
    expect(rows.map(flatRowKey)).toEqual(['g:g2', 'g:g2a', 'n:a', 'n:b']);
  });

  it('hides the ungrouped section when nothing ungrouped survived', () => {
    const t = buildNodeTree([group('g2', 'Sites')], [node('a', 'alpha', 'g2')]);
    expect(flattenTree(t, { collapsed: {}, filter: '', narrowed: true }).map(flatRowKey)).toEqual([
      'g:g2',
      'n:a',
    ]);
    // …and keeps it while browsing, where it is also the drop zone.
    expect(flattenTree(t, { collapsed: {}, filter: '' }).map(flatRowKey)).toContain(
      'ungrouped-head',
    );
  });

  it('still applies the term when both are on', () => {
    // The rows are already state-filtered; the term narrows them further by name. A node the term
    // rejects must go, and its folder with it.
    const t = buildNodeTree(
      [group('g1', 'Japan'), group('g2', 'Sites')],
      [node('a', 'alpha', 'g1'), node('b', 'beta', 'g2')],
    );
    const rows = flattenTree(t, { collapsed: {}, filter: 'alph', narrowed: true });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:a']);
  });

  it('counts the rows on screen, not the fleet', () => {
    // Decided 2026-08-14: while narrowing the bar describes what is shown. "DNS 3" beside a single
    // row makes the operator work out which number is the answer. The server rollup is still the
    // right answer while browsing, where the row stands in for a folder nobody has opened.
    const counts = { g1: { ok: 9, warning: 0, critical: 0, unknown: 0, unreachable: 0, maintenance: 0 },
                     g1a: { ok: 9, warning: 0, critical: 0, unknown: 0, unreachable: 0, maintenance: 0 },
                     g2: { ok: 0, warning: 0, critical: 0, unknown: 0, unreachable: 0, maintenance: 0 },
                     g2a: { ok: 2, warning: 0, critical: 1, unknown: 0, unreachable: 0, maintenance: 0 } };
    const rows = flattenTree(narrowedTree(), {
      collapsed: {},
      filter: '',
      narrowed: true,
      groupCounts: counts,
    });
    const dns = rows.find((r) => flatRowKey(r) === 'g:g2a');
    expect(dns?.kind === 'group' && dns.tally.total).toBe(1);
    // …and browsing still reads the rollup, so an unopened folder is not reported as empty.
    const browsing = flattenTree(narrowedTree(), {
      collapsed: {},
      filter: '',
      groupCounts: counts,
    });
    const dnsBrowsing = browsing.find((r) => flatRowKey(r) === 'g:g2a');
    expect(dnsBrowsing?.kind === 'group' && dnsBrowsing.tally.total).toBe(3);
  });

  it('counts a folder matched by NAME as all of its members', () => {
    // ⚠️ The count has to mirror the row rules, inherited match included: a folder the term matched
    // shows every member, so a count derived from "names that match" would sit beside more rows
    // than it claims.
    const t = buildNodeTree(
      [group('g2', 'Sites'), group('g2a', 'DNS', 'g2')],
      [node('a', 'alpha', 'g2a'), node('b', 'beta', 'g2a', 1)],
    );
    const rows = flattenTree(t, { collapsed: {}, filter: 'dns' });
    const dns = rows.find((r) => flatRowKey(r) === 'g:g2a');
    expect(dns?.kind === 'group' && dns.tally.total).toBe(2);
    expect(rows.map(flatRowKey)).toEqual(['g:g2', 'g:g2a', 'n:a', 'n:b']);
  });

  it('leaves browsing untouched', () => {
    // No flag, no term: the whole tree, collapse state honoured — byte-for-byte what it was.
    // Sibling groups sort by name at equal `sort_order`, so "Internet Sites" precedes "Japan".
    const rows = flattenTree(wide(), { collapsed: {}, filter: '' });
    expect(rows.map(flatRowKey)).toEqual([
      'g:g2',
      'g:g2a',
      'n:n2',
      'g:g1',
      'g:g1a',
      'n:n1',
      'ungrouped-head',
    ]);
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

describe('filterTerm', () => {
  it('trims and lower-cases', () => {
    expect(filterTerm('  ToKyo ')).toBe('tokyo');
  });

  it('reads a blank or whitespace-only box as not filtering', () => {
    expect(filterTerm('')).toBe('');
    expect(filterTerm('   ')).toBe('');
  });
});

describe('mergeNodesById', () => {
  it('concatenates in order and keeps the first entry for a repeated id', () => {
    const a = node('n1', 'sw1', 'g1');
    const b = node('n2', 'sw2', 'g1');
    const dupe = node('n1', 'sw1-stale', 'g1');
    const out = mergeNodesById([a, b], [dupe, node('n3', 'sw3', 'g1')]);
    expect(out.map((n) => n.id)).toEqual(['n1', 'n2', 'n3']);
    // First wins: the same node in both lists must not produce two rows keyed `n:n1`.
    expect(out[0].name).toBe('sw1');
  });
});

describe('revealedGroupKeys', () => {
  const groups = [
    group('g1', 'Tokyo'),
    group('g2', 'Rack A', 'g1'),
    group('g3', 'Rack B', 'g1'),
    group('g4', 'Osaka'),
  ];

  it('reveals nothing while browsing', () => {
    expect(revealedGroupKeys(groups, '   ', 10)).toEqual([]);
  });

  it('reveals a name-matched group and its whole subtree, case-insensitively', () => {
    expect(revealedGroupKeys(groups, 'TOKYO', 10)).toEqual(['g1', 'g2', 'g3']);
  });

  it('reveals nothing for a term that only matches a node name', () => {
    // Nodes are the server search page's job — revealing folders is what this adds.
    expect(revealedGroupKeys(groups, 'sw1', 10)).toEqual([]);
  });

  it('unions two matches and lists an overlapping subtree once', () => {
    const overlapping = [group('g1', 'Tokyo'), group('g2', 'Tokyo Rack', 'g1'), group('g3', 'Osaka')];
    expect(revealedGroupKeys(overlapping, 'tokyo', 10)).toEqual(['g1', 'g2']);
  });

  it('caps the reveal, keeping the deterministic prefix', () => {
    expect(revealedGroupKeys(groups, 'tokyo', 2)).toEqual(['g1', 'g2']);
  });

  it('terminates on cyclic parent links', () => {
    const cyclic = [group('a', 'Alpha', 'b'), group('b', 'Beta', 'a')];
    expect(revealedGroupKeys(cyclic, 'alpha', 10)).toEqual(['a', 'b']);
  });
});

describe('flattenTree — a filter that matches a GROUP reveals its contents', () => {
  const counts = (partial: Partial<Record<NodeState, number>>): Record<NodeState, number> => ({
    ok: 0,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
    ...partial,
  });

  it('shows the matched folder’s members and its sub-folders’ members, matching or not', () => {
    // "Tokyo" matches the folder; not one node name contains it. Osaka is unrelated and stays hidden.
    const t = buildNodeTree(
      [group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1'), group('g3', 'Osaka')],
      [node('n1', 'sw1', 'g2'), node('n2', 'fw1', 'g1'), node('n3', 'sw9', 'g3')],
    );
    const rows = flattenTree(t, { collapsed: { g1: true }, filter: 'tokyo' });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'n:n1', 'n:n2']);
  });

  it('places the loading row after the matches it already has, for a revealed unloaded group', () => {
    // The search page carries sw1 (it matched); the folder's other 4 members are still in flight.
    const t = buildNodeTree([group('g1', 'Tokyo')], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: 'tokyo',
      groupCounts: { g1: counts({ ok: 5 }) },
      loadedGroups: new Set(),
      revealedGroups: new Set(['g1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1', 'loading:g1']);
  });

  it('never draws a loading row for a group the reveal cap left out', () => {
    // Past the cap nothing is being fetched, so a placeholder there would spin forever.
    const t = buildNodeTree([group('g1', 'Tokyo')], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: 'tokyo',
      groupCounts: { g1: counts({ ok: 5 }) },
      loadedGroups: new Set(),
      revealedGroups: new Set(),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1']);
  });

  it('drops the loading row once the revealed group’s members are in', () => {
    // Explicit sort_order pins the member order independent of name.
    const t = buildNodeTree(
      [group('g1', 'Tokyo')],
      [node('n1', 'sw1', 'g1', 1), node('n2', 'fw1', 'g1', 2)],
    );
    const rows = flattenTree(t, {
      collapsed: {},
      filter: 'tokyo',
      groupCounts: { g1: counts({ ok: 2 }) },
      loadedGroups: new Set(['g1']),
      revealedGroups: new Set(['g1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1', 'n:n2']);
  });

  it('ignores the revealed set while browsing (the lazy-load rules are unchanged)', () => {
    const t = buildNodeTree([group('g1', 'Tokyo')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: { g1: counts({ ok: 5 }) },
      loadedGroups: new Set(),
      revealedGroups: new Set(['g1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'loading:g1', 'ungrouped-head']);
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

describe('asGroupType', () => {
  it('passes through every known type', () => {
    for (const gt of GROUP_TYPES) expect(asGroupType(gt)).toBe(gt);
  });

  it('reads an unknown or absent wire value as the generic folder', () => {
    expect(asGroupType('rack')).toBe('generic');
    expect(asGroupType(undefined)).toBe('generic');
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

describe('visibleOpenGroupKeys', () => {
  //   a ── a1 ── a11
  //   b
  const groups = [
    group('a', 'A'),
    group('a1', 'A1', 'a'),
    group('a11', 'A11', 'a1'),
    group('b', 'B'),
  ];

  it('always includes the ungrouped bucket', () => {
    // It has no row to expand, so nothing else would ever ask for it — but it is always on screen.
    expect(visibleOpenGroupKeys([], {})).toEqual([UNGROUPED]);
    expect(visibleOpenGroupKeys(groups, { a: true, b: true })).toEqual([UNGROUPED]);
  });

  it('includes every open group when nothing is collapsed', () => {
    expect(visibleOpenGroupKeys(groups, {}).sort()).toEqual(
      [UNGROUPED, 'a', 'a1', 'a11', 'b'].sort(),
    );
  });

  it('skips a collapsed group and everything beneath it', () => {
    // a1/a11 are still "open" in the prefs, but they are inside a collapsed parent — nothing of
    // theirs is on screen, so fetching their members would be a request for nothing.
    expect(visibleOpenGroupKeys(groups, { a: true }).sort()).toEqual([UNGROUPED, 'b'].sort());
  });

  it('skips a nested collapsed group but keeps its open ancestors', () => {
    expect(visibleOpenGroupKeys(groups, { a1: true }).sort()).toEqual(
      [UNGROUPED, 'a', 'b'].sort(),
    );
  });
});

describe('subtreeGroupIds', () => {
  it('returns the group and every descendant', () => {
    const groups = [
      group('a', 'A'),
      group('a1', 'A1', 'a'),
      group('a2', 'A2', 'a'),
      group('a11', 'A11', 'a1'),
      group('b', 'B'),
    ];
    expect(subtreeGroupIds(groups, 'a').sort()).toEqual(['a', 'a1', 'a11', 'a2'].sort());
    expect(subtreeGroupIds(groups, 'a1').sort()).toEqual(['a1', 'a11'].sort());
    expect(subtreeGroupIds(groups, 'b')).toEqual(['b']);
  });

  it('returns just the id for a group that is not in the list', () => {
    // A stale selection from the URL: it must not throw, and it must not walk the whole fleet.
    expect(subtreeGroupIds([group('a', 'A')], 'gone')).toEqual(['gone']);
  });

  it('terminates on cyclic parent links', () => {
    // This walks the raw parent_id edges as the API returned them, not the built tree, so a cycle
    // the server let through would otherwise spin here and hang the page rather than fail loudly.
    const cyclic = [group('a', 'A', 'b'), group('b', 'B', 'a')];
    expect(subtreeGroupIds(cyclic, 'a').sort()).toEqual(['a', 'b'].sort());
  });
});

describe('findTreeGroup', () => {
  const g = (id: string, children: TreeGroup[] = []): TreeGroup =>
    ({ id, name: id, parent_id: null, children, nodes: [] }) as unknown as TreeGroup;

  it('finds a group at any depth', () => {
    const deep = g('leaf');
    const roots = [g('a', [g('b', [deep])]), g('c')];
    expect(findTreeGroup(roots, 'a')?.id).toBe('a');
    expect(findTreeGroup(roots, 'c')?.id).toBe('c');
    expect(findTreeGroup(roots, 'leaf')).toBe(deep);
  });

  it('returns null rather than throwing for an id that is not in the tree', () => {
    // Reachable: the detail pane keeps a selection while the tree reloads without it.
    expect(findTreeGroup([g('a')], 'gone')).toBeNull();
    expect(findTreeGroup([], 'a')).toBeNull();
  });
});

describe('groupDeletionImpact', () => {
  const grp = (id: string, parent_id: string | null = null) =>
    ({ id, name: id, parent_id }) as NodeGroup;
  const counts = (n: number): StateCounts =>
    ({ ok: n, warning: 0, critical: 0, unknown: 0, unreachable: 0, maintenance: 0 }) as StateCounts;
  // Renders each interpolation inline so an assertion can name the numbers without fighting
  // nested JSON escaping.
  const t = ((key: string, opts?: Record<string, unknown>) =>
    opts && 'count' in opts
      ? `${key}=${opts.count}`
      : `${key}[${Object.values(opts ?? {}).join(',')}]`) as unknown as TFunction;

  it('counts only DIRECT subgroups and the group’s own members', () => {
    const groups = [grp('a'), grp('b', 'a'), grp('c', 'b'), grp('d', 'a')];
    const out = groupDeletionImpact(groups, { a: counts(4) }, grp('a'), t);
    // `c` is a grandchild — it is not counted, which is what the dialog's sentence claims.
    expect(out).toContain('count.subgroup=2');
    expect(out).toContain('count.memberNode=4');
  });

  it('says zero rather than nothing when the roll-up has not arrived', () => {
    // `groupCounts` is fetched separately, so the dialog can open before it lands. Reporting
    // "0 members" is honest; omitting the clause would read as "this group is empty".
    const out = groupDeletionImpact([grp('a')], {}, grp('a'), t);
    expect(out).toContain('count.subgroup=0');
    expect(out).toContain('count.memberNode=0');
  });
});
