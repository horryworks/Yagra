// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  asGroupType,
  buildNodeTree,
  descendantNodes,
  adoptCollapsed,
  adoptFilterTouched,
  filterCollapsedFrom,
  filterGroupOptions,
  filterTerm,
  findTreeGroup,
  foldersWithNodes,
  flatRowKey,
  flattenTree,
  groupDeletionImpact,
  groupOptions,
  groupPath,
  groupTrail,
  isSelfOrDescendant,
  MAX_STORED_COLLAPSED,
  mergeNodesById,
  NO_FILTER_TOUCHED,
  pendingGroupKeys,
  pressTwisty,
  revealedGroupKeys,
  sameNameNodeIds,
  shouldForgetTouched,
  subtreeGroupIds,
  setCollapsed,
  tallyStates,
  touchedFor,
  touchFilter,
  treeFilterKey,
  type StateCounts,
  type TwistyState,
  type TreeGroup,
} from './nodeTree';
import type { TFunction } from 'i18next';
import { pinnedView } from './pins';
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
  tags: [],
  tags_excluded: [],
  effective_tags: [],
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
    expect(g1.kind === 'group' && g1.tally?.total).toBe(1);
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

describe('flattenTree — Pinned only (ADR-146)', () => {
  // Japan ─┬─ Tokyo: tokyo-sw, tokyo-rt
  //        └─ Osaka: osaka-sw
  // US: us-sw                  (ungrouped: lone)
  const groups = [
    group('g1', 'Japan', null, 1),
    group('g1a', 'Tokyo', 'g1', 1),
    group('g1b', 'Osaka', 'g1', 2),
    group('g2', 'US', null, 2),
  ];
  const nodes = [
    node('n1', 'tokyo-sw', 'g1a', 1),
    node('n2', 'tokyo-rt', 'g1a', 2),
    node('n3', 'osaka-sw', 'g1b'),
    node('n4', 'us-sw', 'g2'),
    node('n5', 'lone', null),
  ];
  const view = (pinnedGroups: string[], pinnedNodes: string[]) =>
    pinnedView(
      groups,
      new Set(pinnedGroups),
      new Set(pinnedNodes),
      nodes.filter((n) => pinnedNodes.includes(n.id)),
    );
  const keys = (opts: Partial<Parameters<typeof flattenTree>[1]>) =>
    flattenTree(buildNodeTree(groups, nodes), { collapsed: {}, filter: '', ...opts }).map(flatRowKey);

  it('keeps a pinned node and the folders above it, and nothing beside it', () => {
    expect(keys({ pinned: view([], ['n1']) })).toEqual(['g:g1', 'g:g1a', 'n:n1']);
  });

  it('keeps a pinned folder with everything in it', () => {
    expect(keys({ pinned: view(['g1a'], []) })).toEqual(['g:g1', 'g:g1a', 'n:n1', 'n:n2']);
  });

  it('keeps a pinned ungrouped node under the header, which counts only what it shows', () => {
    const rows = flattenTree(buildNodeTree(groups, nodes), {
      collapsed: {},
      filter: '',
      pinned: view([], ['n5']),
    });
    expect(rows.map(flatRowKey)).toEqual(['ungrouped-head', 'n:n5']);
    expect(rows[0].kind === 'ungrouped-head' && rows[0].count).toBe(1);
  });

  it('reads the saved layout on its own, and the closed folder still counts the pin (ADR-154)', () => {
    // Pinned only is a mode left on across reloads. Before ADR-154 it ignored the saved layout, so
    // an operator who kept it on saw every folder reopen on every visit.
    const rows = flattenTree(buildNodeTree(groups, nodes), {
      collapsed: { g1: true },
      filter: '',
      pinned: view([], ['n1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1']);
    expect(rows[0].kind === 'group' && rows[0].isOpen).toBe(false);
    expect(rows[0].kind === 'group' && rows[0].tally?.total).toBe(1);
  });

  it('still ignores the saved layout once a term is typed over it', () => {
    // The term is a search, and a folder closed while browsing must not hide its match (Inc.6).
    expect(keys({ collapsed: { g1: true }, filter: 'tokyo', pinned: view([], ['n1']) })).toEqual([
      'g:g1',
      'g:g1a',
      'n:n1',
    ]);
  });

  it('combines with a term: only the pinned rows that match', () => {
    expect(keys({ filter: 'sw', pinned: view(['g1a'], ['n4']) })).toEqual([
      'g:g1',
      'g:g1a',
      'n:n1',
      'g:g2',
      'n:n4',
    ]);
  });

  it('combines with a server-side filter: a returned row that is not pinned stays hidden', () => {
    expect(keys({ narrowed: true, pinned: view([], ['n3']) })).toEqual(['g:g1', 'g:g1b', 'n:n3']);
  });

  it('shows no row at all when nothing is pinned', () => {
    expect(keys({ pinned: view([], []) })).toEqual([]);
  });

  it('makes a search under Pinned only a different filter, without changing any other key', () => {
    expect(treeFilterKey('myj', false, '', true)).not.toBe(treeFilterKey('myj', false, ''));
    expect(treeFilterKey('myj', false, '')).toBe(JSON.stringify(['myj', '']));
  });
});

describe('flattenTree — Folders with nodes only (ADR-159)', () => {
  // Japan ─┬─ Tokyo: tokyo-sw          (the folder with a node)
  //        └─ Osaka                    (empty, and nothing below it)
  // US ────── Austin                   (US is empty itself; the node is one level down)
  // Spare                              (empty, no children)          (ungrouped: lone)
  const groups = [
    group('g1', 'Japan', null, 1),
    group('g1a', 'Tokyo', 'g1', 1),
    group('g1b', 'Osaka', 'g1', 2),
    group('g2', 'US', null, 2),
    group('g2a', 'Austin', 'g2', 1),
    group('g3', 'Spare', null, 3),
  ];
  const nodes = [node('n1', 'tokyo-sw', 'g1a', 1), node('n2', 'austin-sw', 'g2a', 1)];
  const counts = (ok: number): StateCounts => ({
    ok,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
  });
  // What the server answers: a folder with no members of its own simply never appears.
  const groupCounts = { g1a: counts(1), g2a: counts(1) };
  const keys = (opts: Partial<Parameters<typeof flattenTree>[1]>) =>
    flattenTree(buildNodeTree(groups, nodes), {
      collapsed: {},
      filter: '',
      groupCounts,
      loadedGroups: new Set(['g1a', 'g2a']),
      ...opts,
    }).map(flatRowKey);

  it('drops a folder with nothing below it, and keeps the one whose node is a level down', () => {
    expect(keys({ withNodesOnly: { keep: new Set() } })).toEqual([
      'g:g1',
      'g:g1a',
      'n:n1',
      'g:g2',
      'g:g2a',
      'n:n2',
      'ungrouped-head',
    ]);
  });

  it('keeps every folder while the switch is off', () => {
    expect(keys({})).toContain('g:g1b');
    expect(keys({})).toContain('g:g3');
  });

  it('keeps a folder just created, and the folders above it, though it is empty', () => {
    // Without this an operator presses "New folder" and the tree does not change (ADR-055 R6).
    const rows = keys({ withNodesOnly: { keep: new Set(['g1b']) } });
    expect(rows).toContain('g:g1b');
    expect(rows).toContain('g:g1');
    expect(rows).not.toContain('g:g3');
  });

  it('hides nothing while the counts are still on their way', () => {
    // 🚨 The failure this guards: judged against counts that have not arrived, EVERY folder reads
    // as empty — so the whole tree would blank for a round trip on every visit.
    const rows = keys({
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(),
      withNodesOnly: { keep: new Set() },
    });
    for (const g of ['g:g1', 'g:g1a', 'g:g1b', 'g:g2', 'g:g2a', 'g:g3']) expect(rows).toContain(g);
  });

  it('combines with Pinned only: a pinned folder with nothing in it is still dropped', () => {
    const view = pinnedView(groups, new Set(['g1b', 'g1a']), new Set(), []);
    expect(keys({ pinned: view, withNodesOnly: { keep: new Set() } })).toEqual([
      'g:g1',
      'g:g1a',
      'n:n1',
    ]);
  });

  it('combines with a term: an empty folder whose name matches is dropped too', () => {
    expect(keys({ filter: 'osaka', withNodesOnly: { keep: new Set() } })).toEqual([]);
  });

  it('reads the saved layout, because it is not a search (ADR-154 decision 7)', () => {
    const rows = flattenTree(buildNodeTree(groups, nodes), {
      collapsed: { g1: true },
      filter: '',
      groupCounts,
      loadedGroups: new Set(['g1a', 'g2a']),
      withNodesOnly: { keep: new Set() },
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'g:g2a', 'n:n2', 'ungrouped-head']);
    expect(rows[0].kind === 'group' && rows[0].isOpen).toBe(false);
    // The closed folder still counts what is under it — that is how the operator finds it.
    expect(rows[0].kind === 'group' && rows[0].tally?.total).toBe(1);
  });

  it('judges by the loaded members on the legacy path, where there are no server counts', () => {
    const rows = flattenTree(buildNodeTree(groups, nodes), {
      collapsed: {},
      filter: '',
      withNodesOnly: { keep: new Set() },
    }).map(flatRowKey);
    expect(rows).toContain('g:g1a');
    expect(rows).not.toContain('g:g1b');
  });
});

describe('foldersWithNodes (ADR-159)', () => {
  const groups = [
    group('g1', 'Japan'),
    group('g1a', 'Tokyo', 'g1'),
    group('g1b', 'Osaka', 'g1'),
    group('g2', 'US'),
  ];
  const tally = (total: number) => ({
    counts: emptyCounts(),
    total,
    needAttention: 0,
  });
  const emptyCounts = (): Record<NodeState, number> => ({
    ok: 0,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
  });

  it('keeps a folder above a kept one even when a sibling is kept first', () => {
    // 🚨 The bug this guards: `below || visit(child)` short-circuits, so once Tokyo answers "keep"
    // Osaka is never visited — and a folder kept only by `keep` under it disappears.
    const tree = buildNodeTree(groups, []);
    const sub = new Map([
      ['g1', tally(1)],
      ['g1a', tally(1)],
      ['g1b', tally(0)],
      ['g2', tally(0)],
    ]);
    const kept = foldersWithNodes(tree.roots, sub, new Set(['g1b']));
    expect([...kept].sort()).toEqual(['g1', 'g1a', 'g1b']);
  });

  it('falls back to the loaded members when there is no rollup', () => {
    const tree = buildNodeTree(groups, [node('n1', 'sw', 'g1b')]);
    expect([...foldersWithNodes(tree.roots, null, new Set())].sort()).toEqual(['g1', 'g1b']);
  });
});

describe('flattenTree — Pinned only over the lazy tree (ADR-146)', () => {
  const counts = (ok: number): Record<NodeState, number> => ({
    ok,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
  });
  const groups = [group('g1', 'Japan'), group('g1a', 'Tokyo', 'g1', 1), group('g1b', 'Osaka', 'g1', 2)];
  const groupCounts = { g1: counts(4), g1a: counts(2), g1b: counts(3) };

  it('asks for a pinned folder\'s members, and never for the folder above it', () => {
    // 🚨 The failure this guards: had Pinned only borrowed the search rules, the pinned folder would
    // read as loaded and empty — no placeholder, so nothing fetched, and no row to put members in.
    const rows = flattenTree(buildNodeTree(groups, []), {
      collapsed: {},
      filter: '',
      groupCounts,
      loadedGroups: new Set(),
      pinned: pinnedView(groups, new Set(['g1b']), new Set(), []),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g1b', 'loading:g1b']);
    // The pinned folder is shown whole, so its bar is its whole membership; the folder above counts
    // only that folder, not its own 4 or Tokyo's 2.
    expect(rows[1].kind === 'group' && rows[1].tally?.total).toBe(3);
    expect(rows[0].kind === 'group' && rows[0].tally?.total).toBe(3);
  });

  it('adds a pinned node filed directly in a folder above to that folder\'s count', () => {
    const core = node('n9', 'core', 'g1', 0, 'critical');
    const rows = flattenTree(buildNodeTree(groups, [core]), {
      collapsed: {},
      filter: '',
      groupCounts,
      loadedGroups: new Set(),
      pinned: pinnedView(groups, new Set(['g1b']), new Set(['n9']), [core]),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g1b', 'loading:g1b', 'n:n9']);
    const top = rows[0];
    expect(top.kind === 'group' && top.tally?.total).toBe(4);
    expect(top.kind === 'group' && top.tally?.counts.critical).toBe(1);
  });

  it('draws the counts as unknown while they are still on their way', () => {
    const rows = flattenTree(buildNodeTree(groups, []), {
      collapsed: {},
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(),
      pinned: pinnedView(groups, new Set(['g1b']), new Set(), []),
    });
    expect(rows[0].kind === 'group' && rows[0].tally).toBeNull();
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g1b', 'loading:g1b']);
  });
});

describe('flattenTree — collapsing a folder while a filter is on (ADR-053 Inc.11)', () => {
  // The reported bug: with `MYJ` in the box, the folder's ▼ did nothing. Filtering forced every
  // folder open, and the twisty wrote a set filtering never reads.
  const tree = () =>
    buildNodeTree(
      [group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1')],
      [node('n1', 'sw1', 'g2'), node('n2', 'router', null)],
    );

  it('a folder collapsed under a term hides what is under it and keeps its own row', () => {
    const rows = flattenTree(tree(), { collapsed: {}, filter: 'sw1', filterCollapsed: { g2: true } });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g2']);
    const g2 = rows[1];
    expect(g2.kind === 'group' && g2.isOpen).toBe(false);
    // The twisty has to stay enabled, or the folder could be closed and never opened again.
    expect(g2.kind === 'group' && g2.hasChildren).toBe(true);
  });

  it('works for a state filter with an empty box, too', () => {
    const narrowedTree = buildNodeTree(
      [group('g1', 'Internet Sites'), group('g2', 'DNS', 'g1')],
      [node('n1', 'test.example', 'g2', 0, 'critical')],
    );
    const rows = flattenTree(narrowedTree, {
      collapsed: {},
      filter: '',
      narrowed: true,
      filterCollapsed: { g1: true },
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1']);
  });

  it('still ignores the saved layout while filtering, and the filter set while browsing', () => {
    // Both directions: the saved set must not hide a match (Inc.6), and a set left over from a
    // filter must not close a folder in the tree the operator browses.
    const filtering = flattenTree(tree(), {
      collapsed: { g1: true, g2: true },
      filter: 'sw1',
      filterCollapsed: {},
    });
    expect(filtering.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'n:n1']);
    const browsing = flattenTree(tree(), { collapsed: {}, filter: '', filterCollapsed: { g1: true } });
    expect(browsing.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'n:n1', 'ungrouped-head', 'n:n2']);
  });
});

describe('the saved layout a twisty writes (ADR-154)', () => {
  it('setCollapsed hands back the same object when nothing changes', () => {
    // Load-bearing: the caller saves to the account only when the reference moved.
    const set = { g1: true } as const;
    expect(setCollapsed(set, 'g1', true)).toBe(set);
    expect(setCollapsed(set, 'g2', false)).toBe(set);
    expect(setCollapsed(set, 'g2', true)).toEqual({ g1: true, g2: true });
    expect(setCollapsed(set, 'g1', false)).toEqual({});
    expect(set).toEqual({ g1: true });
  });

  it('drops the folder closed longest ago past the cap, never the one just closed', () => {
    let set: Readonly<Record<string, true>> = {};
    for (let i = 0; i < MAX_STORED_COLLAPSED; i += 1) set = setCollapsed(set, `g${i}`, true);
    const next = setCollapsed(set, 'newest', true);
    expect(Object.keys(next)).toHaveLength(MAX_STORED_COLLAPSED);
    expect(next.g0).toBeUndefined();
    expect(next.g1).toBe(true);
    expect(next.newest).toBe(true);
  });

  it('adoptCollapsed keeps only true-valued ids of a plausible length, and caps them', () => {
    expect(adoptCollapsed({ a: true, b: false, c: 'true', d: 1, '': true, [`x${'y'.repeat(64)}`]: true }))
      .toEqual({ a: true });
    const many: Record<string, true> = {};
    for (let i = 0; i < MAX_STORED_COLLAPSED + 5; i += 1) many[`g${i}`] = true;
    const adopted = adoptCollapsed(many);
    expect(Object.keys(adopted ?? {})).toHaveLength(MAX_STORED_COLLAPSED);
    expect(adopted?.[`g${MAX_STORED_COLLAPSED + 4}`]).toBe(true);
  });

  it('adoptCollapsed tells "no layout" from "an empty layout"', () => {
    // `null` keeps this browser's layout; `{}` is the account saying everything is open.
    for (const raw of [undefined, null, 'x', 42, true, [], ['g1']]) expect(adoptCollapsed(raw)).toBeNull();
    expect(adoptCollapsed({})).toEqual({});
  });
});

describe('what a filter remembers about presses (ADR-053 Inc.11, ADR-154)', () => {
  it('is kept under the same filter and dropped under a different one', () => {
    const myj = treeFilterKey('MYJ', false, '');
    const held = touchFilter(NO_FILTER_TOUCHED, myj, 'g1');
    expect(touchedFor(held, myj)).toEqual({ g1: true });
    expect(touchedFor(held, treeFilterKey('MYJ0', false, ''))).toEqual({});
    expect(touchFilter(held, myj, 'g1')).toBe(held);
  });

  it('a press under a new filter starts from nothing pressed, not from the old set', () => {
    const first = touchFilter(NO_FILTER_TOUCHED, treeFilterKey('tokyo', false, ''), 'g1');
    const second = touchFilter(first, treeFilterKey('osaka', false, ''), 'g2');
    expect(second.ids).toEqual({ g2: true });
  });

  it('shows closed only what was pressed here AND is closed in the saved layout', () => {
    // g1 was closed while browsing and never pressed here: open, which is Inc.6.
    // g2 was pressed here and is closed: closed. g3 was pressed twice: open again.
    expect(filterCollapsedFrom({ g1: true, g2: true }, { g2: true, g3: true })).toEqual({ g2: true });
    expect(filterCollapsedFrom({ g1: true }, {})).toBe(filterCollapsedFrom({}, {}));
  });

  it('keys on the term as the tree compares it, and on the server-side values', () => {
    expect(treeFilterKey(' MYJ ', false, '')).toBe(treeFilterKey('myj', false, ''));
    // `narrowed` is true for both, so only the values can tell these two questions apart.
    expect(treeFilterKey('', true, 'critical  ')).not.toBe(treeFilterKey('', true, 'warning  '));
    // Values the tree was not told are narrowing it do not make a different filter.
    expect(treeFilterKey('x', false, 'critical  ')).toBe(treeFilterKey('x', false, ''));
  });
});

describe('the pressed-folder record a tab keeps across a reload (ADR-154 increment 2)', () => {
  it('reads back a well-formed record, and drops what does not read as one', () => {
    const key = treeFilterKey('region', false, '');
    expect(adoptFilterTouched({ key, ids: { g1: true, g2: false, g3: 'true' } })).toEqual({
      key,
      ids: { g1: true },
    });
    for (const raw of [
      undefined,
      null,
      'x',
      [],
      { ids: { g1: true } },
      { key: '', ids: { g1: true } },
      { key: 42, ids: { g1: true } },
      { key, ids: 'g1' },
      { key, ids: {} },
      { key, ids: { g1: false } },
    ]) {
      // The shared empty value, not merely an equal one: the tree compares by reference.
      expect(adoptFilterTouched(raw)).toBe(NO_FILTER_TOUCHED);
    }
  });

  it('caps the ids like the saved layout', () => {
    const ids: Record<string, true> = {};
    for (let i = 0; i < MAX_STORED_COLLAPSED + 3; i += 1) ids[`g${i}`] = true;
    expect(Object.keys(adoptFilterTouched({ key: 'k', ids }).ids)).toHaveLength(MAX_STORED_COLLAPSED);
  });

  it('is forgotten only when the page has not asked for a search AND the tree shows none', () => {
    // 🚨 The first render after a reload: the page holds `?q=` already, the tree has not been handed
    // the term yet. Forgetting here is what would make the record useless.
    expect(shouldForgetTouched(false, true), 'forgot the record on the first frame of a reload').toBe(
      false,
    );
    // The box was just cleared: the page's term is empty a tick before the tree stops searching.
    expect(shouldForgetTouched(true, false)).toBe(false);
    expect(shouldForgetTouched(true, true)).toBe(false);
    // Both agree the search is gone: typing the same term again starts every folder open.
    expect(shouldForgetTouched(false, false)).toBe(true);
  });
});

describe('pressing a twisty, end to end (ADR-154)', () => {
  // Tokyo ─ Rack A: sw1        (ungrouped: router)
  const tree = () =>
    buildNodeTree(
      [group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1')],
      [node('n1', 'sw1', 'g2'), node('n2', 'router', null)],
    );
  const key = (term: string) => treeFilterKey(term, false, '');

  /** Press the twisty of `id` as the row currently shows it, the way `NodeTree.tsx` does. */
  const press = (s: TwistyState, id: string, term: string): TwistyState => {
    const searching = term.trim().length > 0;
    const row = view(s, term).find((r) => r.kind === 'group' && r.group.id === id);
    if (row?.kind !== 'group') throw new Error(`no row for ${id}`);
    return pressTwisty(s, { id, isOpen: row.isOpen, searching, key: key(term) });
  };
  /** What the tree draws for `term` — the touched set as the component reads it. */
  const view = (s: TwistyState, term: string) =>
    flattenTree(tree(), {
      collapsed: s.collapsed,
      filter: term,
      filterCollapsed: filterCollapsedFrom(s.collapsed, touchedFor(s.touched, key(term))),
    });
  /** Leaving the filter, as the component does: the pressed set is forgotten, the layout is not. */
  const clear = (s: TwistyState): TwistyState => ({ ...s, touched: NO_FILTER_TOUCHED });
  const start: TwistyState = { collapsed: {}, touched: NO_FILTER_TOUCHED };
  const shown = (s: TwistyState, term: string) => view(s, term).map(flatRowKey);

  it('a folder closed under a filter is still closed after the filter is cleared (the report)', () => {
    let s = press(start, 'g2', 'tokyo');
    expect(shown(s, 'tokyo'), 'the press did not close the folder under the filter').toEqual([
      'g:g1',
      'g:g2',
    ]);
    s = clear(s);
    expect(shown(s, ''), 'clearing the filter reopened the folder').toEqual([
      'g:g1',
      'g:g2',
      'ungrouped-head',
      'n:n2',
    ]);
  });

  it('a folder closed while browsing shows open under a filter, and one press closes it', () => {
    // A flip here would REMOVE g2 from the saved layout while the operator watched it close.
    let s = press(start, 'g2', '');
    expect(shown(s, 'tokyo')).toEqual(['g:g1', 'g:g2', 'n:n1']);
    const before = s.collapsed;
    s = press(s, 'g2', 'tokyo');
    expect(shown(s, 'tokyo')).toEqual(['g:g1', 'g:g2']);
    expect(s.collapsed, 'closing an already-closed folder wrote the layout').toBe(before);
  });

  it('opening it again under the filter leaves it open after the filter is cleared', () => {
    let s = press(start, 'g2', '');
    s = press(s, 'g2', 'tokyo');
    s = press(s, 'g2', 'tokyo');
    expect(shown(s, 'tokyo')).toEqual(['g:g1', 'g:g2', 'n:n1']);
    expect(shown(clear(s), '')).toEqual(['g:g1', 'g:g2', 'n:n1', 'ungrouped-head', 'n:n2']);
  });

  it('a different filter starts with every folder open, and keeps the layout underneath', () => {
    let s = press(start, 'g2', 'tokyo');
    expect(shown(s, 'rack'), 'the new filter inherited the old one\'s closed folder').toEqual([
      'g:g1',
      'g:g2',
      'n:n1',
    ]);
    // Accepted cost (ADR-154): the folder was last SEEN open under `rack`, and is closed after.
    s = clear(s);
    expect(shown(s, '')).toEqual(['g:g1', 'g:g2', 'ungrouped-head', 'n:n2']);
  });

  it('a press while browsing writes the layout and records nothing', () => {
    const s = press(start, 'g1', '');
    expect(s.collapsed).toEqual({ g1: true });
    expect(s.touched).toBe(NO_FILTER_TOUCHED);
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
    expect(dns?.kind === 'group' && dns.tally?.total).toBe(1);
    // …and browsing still reads the rollup, so an unopened folder is not reported as empty.
    const browsing = flattenTree(narrowedTree(), {
      collapsed: {},
      filter: '',
      groupCounts: counts,
    });
    const dnsBrowsing = browsing.find((r) => flatRowKey(r) === 'g:g2a');
    expect(dnsBrowsing?.kind === 'group' && dnsBrowsing.tally?.total).toBe(3);
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
    expect(dns?.kind === 'group' && dns.tally?.total).toBe(2);
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
    expect(g1?.kind === 'group' && g1.tally?.total).toBe(3);
    expect(g1?.kind === 'group' && g1.tally?.counts.critical).toBe(1);
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

  it('emits a FAILED row, not a loading one, for a group whose fetch failed', () => {
    // The two must be distinguishable (ADR-125). Nothing retries a failed group on its own any
    // more, so drawing it as "loading" leaves the operator waiting on something never coming.
    const t = buildNodeTree([group('g1', 'Tokyo')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: { g1: counts({ ok: 5 }) },
      loadedGroups: new Set(),
      failedGroups: new Set(['g1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'failed:g1', 'ungrouped-head']);
  });

  it('leaves a group that has not failed on the loading row', () => {
    // The other direction: a failed set naming some *other* group must not repaint this one.
    const t = buildNodeTree([group('g1', 'Tokyo')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: { g1: counts({ ok: 5 }) },
      loadedGroups: new Set(),
      failedGroups: new Set(['g2']),
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

describe('flattenTree — the counts are still on their way (ADR-133)', () => {
  it('asks for the members of every open, unloaded folder before any count has arrived', () => {
    // 🚨 The one that matters. The fetch set is `pendingGroupKeys` over these rows, so if a
    // count-less skeleton emits no `group-loading` row, the tree paints and then fetches NOTHING —
    // which is what a progressive first paint would have shipped, and what a failing
    // `/fleet/group-summary` was already doing in production.
    const t = buildNodeTree([group('g1', 'Tokyo', null, 1), group('g2', 'Osaka', null, 2)], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(),
    });
    expect(rows.map(flatRowKey)).toEqual([
      'g:g1',
      'loading:g1',
      'g:g2',
      'loading:g2',
      'ungrouped-head',
    ]);
    expect(pendingGroupKeys(rows)).toEqual(['g1', 'g2']);
  });

  it('asks for nothing when the same empty counts are a real answer', () => {
    // The other half, and the reason the flag exists rather than a test on `{}` being empty:
    // `{}` WITHOUT the flag means "answered: every folder is empty", and an empty folder must not
    // be fetched. The two inputs differ by one boolean and must behave oppositely.
    const t = buildNodeTree([group('g1', 'Tokyo', null, 1), group('g2', 'Osaka', null, 2)], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: {},
      loadedGroups: new Set(),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'g:g2', 'ungrouped-head']);
    expect(pendingGroupKeys(rows)).toEqual([]);
  });

  it('reports an unknown tally as null rather than as zero', () => {
    // A zero tally renders as an empty bar beside a `0`, which an operator reads as "empty folder".
    // `null` is what lets the row draw a skeleton instead.
    const t = buildNodeTree([group('g1', 'Tokyo')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(),
    });
    const g1 = rows.find((r) => flatRowKey(r) === 'g:g1');
    expect(g1?.kind === 'group' && g1.tally).toBeNull();
    // And the twisty is offered: we cannot yet know the folder is empty, and refusing to open one
    // that has members is the worse mistake.
    expect(g1?.kind === 'group' && g1.hasChildren).toBe(true);
  });

  it('offers a twisty on a folder whose members arrived while the counts never did', () => {
    // The degraded steady state after `/fleet/group-summary` fails outright: members loaded, counts
    // absent forever. Without the loaded-member term in `hasChildren` the twisty is DISABLED on an
    // already-open folder — open, with no way to close it.
    const t = buildNodeTree([group('g1', 'Tokyo')], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(['g1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1', 'ungrouped-head']);
    const g1 = rows.find((r) => flatRowKey(r) === 'g:g1');
    expect(g1?.kind === 'group' && g1.hasChildren).toBe(true);
  });

  it('leaves a loaded folder alone: no placeholder, and it drops out of the fetch set', () => {
    const t = buildNodeTree([group('g1', 'Tokyo', null, 1), group('g2', 'Osaka', null, 2)], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(['g1']),
    });
    expect(pendingGroupKeys(rows)).toEqual(['g2']);
  });

  it('does not make a narrowed tree report a null tally', () => {
    // Narrowing counts the rows it is about to draw, which needs no server answer — so the pending
    // flag must not reach it. A null here would blank the bar during a search.
    const t = buildNodeTree([group('g1', 'Tokyo')], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: 'sw',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(['g1']),
    });
    const g1 = rows.find((r) => flatRowKey(r) === 'g:g1');
    expect(g1?.kind === 'group' && g1.tally?.total).toBe(1);
  });

  it('leaves a collapsed folder out of the fetch set while pending', () => {
    // The property ADR-125 bought and this must not spend: collapsed means not fetched. A pending
    // flag that reached every folder would put the 501-request first paint back.
    const t = buildNodeTree([group('g1', 'Tokyo', null, 1), group('g2', 'Osaka', null, 2)], []);
    const rows = flattenTree(t, {
      collapsed: { g2: true },
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(),
    });
    expect(pendingGroupKeys(rows)).toEqual(['g1']);
  });

  it('leaves the legacy full-node path untouched', () => {
    // No counts, no loaded set, no flag: tally comes from the loaded descendants and every group
    // counts as loaded. Byte-for-byte what it was.
    const t = buildNodeTree([group('g1', 'Tokyo')], [node('n1', 'sw1', 'g1')]);
    const rows = flattenTree(t, { collapsed: {}, filter: '' });
    const g1 = rows.find((r) => flatRowKey(r) === 'g:g1');
    expect(g1?.kind === 'group' && g1.tally?.total).toBe(1);
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'n:n1', 'ungrouped-head']);
  });

  it('still refuses to draw a failed folder as a loading one while pending', () => {
    const t = buildNodeTree([group('g1', 'Tokyo')], []);
    const rows = flattenTree(t, {
      collapsed: {},
      filter: '',
      groupCounts: {},
      countsPending: true,
      loadedGroups: new Set(),
      failedGroups: new Set(['g1']),
    });
    expect(rows.map(flatRowKey)).toEqual(['g:g1', 'failed:g1', 'ungrouped-head']);
    // …and a failed folder is never in the fetch set, pending or not (ADR-125 decision 2).
    expect(pendingGroupKeys(rows)).toEqual([]);
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
  it('the collator orders names exactly as localeCompare does', () => {
    // 🚨 The guard on ADR-133's one visible risk. `byOrder` swapped `a.name.localeCompare(b.name)`
    // for a shared `Intl.Collator`, which is the same comparison ONLY while the collator is built
    // with no options — add `numeric: true` or a `sensitivity` and every operator's tree silently
    // re-orders. Nothing else in the suite would notice: both orders look plausible.
    //
    // The names are chosen to separate the options that would matter: digits (`numeric`), case
    // (`sensitivity`/`caseFirst`), accents, and a non-Latin script.
    const names = [
      'sw10',
      'sw2',
      'SW1',
      'sw1',
      'Ärger',
      'arger',
      'ZZZ',
      'あ',
      'router-1',
      'Router-10',
      'router-2',
    ];
    const viaCollator = names.map((n, i) => ({ id: String(i), name: n, sort_order: 0 }));
    const tree = buildNodeTree(
      [],
      viaCollator.map((g) => ({
        id: g.id,
        name: g.name,
        address: '10.0.0.1',
        state: 'ok' as const,
        vendor: null,
        model: null,
        group_id: null,
        sort_order: 0,
        kind: 'device' as const,
      })),
    );
    expect(tree.ungrouped.map((n) => n.name)).toEqual(
      [...names].sort((a, b) => a.localeCompare(b)),
    );
  });

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

  it('keeps every folder above every node, whatever the sort_order values are', () => {
    // 🚨 A property of the **structure**, not of the comparator: a `TreeGroup` holds folders and
    // nodes in two separate arrays and `flattenTree` walks the children before the members, so the
    // two can never interleave. The ADR-130 sort commands lean on this — they renumber the two
    // scopes independently and never compare a folder against a node — so merging the two lists
    // into one ordered array would make "Sort descending" interleave them, and no other test here
    // would notice.
    //
    // Chosen so the assertion can only hold structurally: the folder sorts last by name *and* last
    // by sort_order, the node first on both. One merged list would put the node on top.
    const tree = buildNodeTree(
      [group('p', 'parent'), group('c', 'zzz', 'p', 99)],
      [node('n1', 'aaa', 'p', 1)],
    );
    expect(flattenTree(tree, { collapsed: {}, filter: '' }).map(flatRowKey)).toEqual([
      'g:p',
      'g:c',
      'n:n1',
      // The Ungrouped header is always emitted while there is any inventory — it is the root drop
      // zone, not a row about these three.
      'ungrouped-head',
    ]);
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

describe('groupTrail', () => {
  const groups = [group('a', 'Tokyo'), group('b', 'Edge', 'a'), group('c', 'Firewall', 'b')];

  it('carries each folder id with its name, root first, so a segment can be opened', () => {
    expect(groupTrail(groups, 'c')).toEqual([
      { id: 'a', name: 'Tokyo' },
      { id: 'b', name: 'Edge' },
      { id: 'c', name: 'Firewall' },
    ]);
  });

  it('is the path groupPath draws, segment for segment', () => {
    for (const id of ['a', 'b', 'c']) {
      expect(groupTrail(groups, id).map((s) => s.name)).toEqual(groupPath(groups, id));
    }
  });

  it('is empty for a null or unknown id', () => {
    expect(groupTrail(groups, null)).toEqual([]);
    expect(groupTrail(groups, 'missing')).toEqual([]);
  });

  it('stops on cyclic parent links instead of looping', () => {
    const cyclic = [group('x', 'X', 'y'), group('y', 'Y', 'x')];
    expect(groupTrail(cyclic, 'x').length).toBeLessThanOrEqual(cyclic.length + 1);
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
  const tree = [
    group('a', 'Tokyo'),
    group('b', 'Osaka'),
    group('a1', 'Edge', 'a'),
    group('a2', 'Core', 'a'),
  ];

  it('flattens the hierarchy into depth-carrying, name-sorted options', () => {
    const opts = groupOptions(tree);
    // Top-level groups name-sorted, each parent's children following it.
    expect(opts.map((o) => o.label)).toEqual(['Osaka', 'Tokyo', 'Core', 'Edge']);
    expect(opts.map((o) => o.depth)).toEqual([0, 0, 1, 1]);
  });

  it('carries the depth as data rather than as spaces in the label', () => {
    // 🚨 It used to indent by prepending two full-width spaces per level (ADR-124 決定 9). That
    // made the depth un-styleable and, worse, put invisible characters into the text a search
    // term is matched against — so a picker filtering on the label would compare against padding.
    for (const o of groupOptions(tree)) {
      expect(o.label).toBe(o.label.trim());
    }
  });

  it('gives every folder its full path from the root', () => {
    // The path is what the picker shows once a filter has removed the parents the indent was
    // measured from, and what the trigger shows for a chosen folder — two sites can both hold a
    // rack called "R1".
    expect(groupOptions(tree).map((o) => o.path)).toEqual([
      'Osaka',
      'Tokyo',
      'Tokyo / Core',
      'Tokyo / Edge',
    ]);
  });
});

describe('filterGroupOptions', () => {
  const opts = groupOptions([
    group('a', 'Tokyo'),
    group('b', 'Osaka'),
    group('a1', 'Edge', 'a'),
    group('a2', 'Core', 'a'),
  ]);

  it('matches the whole path, so a site keeps the racks under it', () => {
    // "show me Tokyo" means the site and everything in it, not the one row whose own name matches.
    expect(filterGroupOptions(opts, 'tokyo').map((o) => o.label)).toEqual([
      'Tokyo',
      'Core',
      'Edge',
    ]);
  });

  it('ignores case', () => {
    expect(filterGroupOptions(opts, 'EDGE').map((o) => o.label)).toEqual(['Edge']);
  });

  it('treats an empty or blank term as no filter', () => {
    // A naive `includes('')` walk would be right by accident here; a naive `trim()` check that
    // returned [] would empty the list the moment the operator pressed space.
    expect(filterGroupOptions(opts, '')).toHaveLength(4);
    expect(filterGroupOptions(opts, '   ')).toHaveLength(4);
  });

  it('answers with nothing when nothing matches', () => {
    expect(filterGroupOptions(opts, 'nagoya')).toEqual([]);
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

describe('pendingGroupKeys', () => {
  // The rows the virtualizer is showing decide what gets fetched (ADR-125). Building them through
  // `flattenTree` rather than by hand is the point: the three rules that decide whether a folder
  // gets a loading row (open, unloaded, non-empty) then have exactly one implementation.
  const counts = (partial: Partial<Record<NodeState, number>>): Record<NodeState, number> => ({
    ok: 0,
    warning: 0,
    critical: 0,
    unreachable: 0,
    maintenance: 0,
    unknown: 0,
    ...partial,
  });
  const rowsFor = (opts: { loaded?: string[]; failed?: string[]; collapsed?: string[] }) =>
    flattenTree(
      buildNodeTree(
        [group('g1', 'Tokyo', null, 1), group('g2', 'Osaka', null, 2), group('g3', 'Empty')],
        [node('n1', 'sw1', 'g1')],
      ),
      {
        collapsed: Object.fromEntries((opts.collapsed ?? []).map((id) => [id, true])),
        filter: '',
        groupCounts: { g1: counts({ ok: 1 }), g2: counts({ ok: 4 }), g3: counts({}) },
        loadedGroups: new Set(opts.loaded ?? []),
        failedGroups: new Set(opts.failed ?? []),
      },
    );

  it('names the groups that are waiting for members', () => {
    expect(pendingGroupKeys(rowsFor({ loaded: ['g1'] }))).toEqual(['g2']);
  });

  it('leaves out a group that is already loaded', () => {
    expect(pendingGroupKeys(rowsFor({ loaded: ['g1', 'g2'] }))).toEqual([]);
  });

  it('leaves out an empty group — there is nothing to ask for', () => {
    // g3 has zero direct members in the server counts, so it never gets a loading row.
    expect(pendingGroupKeys(rowsFor({ loaded: ['g1', 'g2'] }))).not.toContain('g3');
  });

  it('leaves out a collapsed group', () => {
    expect(pendingGroupKeys(rowsFor({ loaded: ['g1'], collapsed: ['g2'] }))).toEqual([]);
  });

  it('🚨 leaves out a FAILED group — nothing retries on its own', () => {
    // Including it here would put the automatic retry back one level up, where the queue could not
    // see it either. A failed folder is fetched again only when the operator presses retry.
    expect(pendingGroupKeys(rowsFor({ loaded: ['g1'], failed: ['g2'] }))).toEqual([]);
  });

  it('ignores group and node rows', () => {
    // Only the placeholder rows are the fetch set; a `group` row is drawn from the server counts
    // and says nothing about whether its members are wanted.
    const rows = rowsFor({ loaded: ['g1', 'g2'] });
    expect(rows.some((r) => r.kind === 'group')).toBe(true);
    expect(rows.some((r) => r.kind === 'node')).toBe(true);
    expect(pendingGroupKeys(rows)).toEqual([]);
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

describe('sameNameNodeIds', () => {
  const ids = (nodes: NodeSummary[]) => [...sameNameNodeIds(nodes)].sort();

  it('marks every node that shares its name with another in the same folder', () => {
    // The PoC box's duplicates: two `as001` rows in one folder, indistinguishable by name.
    expect(ids([node('a', 'as001', 'g1'), node('b', 'as001', 'g1'), node('c', 'as002', 'g1')])).toEqual(
      ['a', 'b'],
    );
  });

  it('does not mark the same name in two different folders', () => {
    expect(ids([node('a', 'core-sw', 'g1'), node('b', 'core-sw', 'g2')])).toEqual([]);
  });

  it('treats the tree root as one folder', () => {
    expect(ids([node('a', 'edge', null), node('b', 'edge', null), node('c', 'edge', 'g1')])).toEqual(
      ['a', 'b'],
    );
  });

  it('compares names exactly', () => {
    expect(ids([node('a', 'AS001', 'g1'), node('b', 'as001', 'g1')])).toEqual([]);
  });

  it('does not count one node listed twice as a duplicate of itself', () => {
    expect(ids([node('a', 'as001', 'g1'), node('a', 'as001', 'g1')])).toEqual([]);
  });
});
