// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type {
  MaintenanceWindow,
  NodeGroup,
  NodeSummary,
  SuppressionExemption,
} from '../types/api';
import {
  buildSuppressionIndex,
  CAUSE_LABEL_KEYS,
  causesFor,
  groupContainers,
  groupSubtree,
  MAX_TIMER_DELAY_MS,
  nextSuppressionExpiry,
  suppressionRefreshDelayMs,
  PANEL_LABEL_KEYS,
  suppressionPanelRows,
  type SuppressionPanelRow,
} from './suppression';

// Tree: tokyo ⊃ edge ⊃ (n2); tokyo also holds n1. osaka is a separate top-level group with n3.
const groups: NodeGroup[] = [
  { id: 'tokyo', name: 'Tokyo', group_type: 'site', parent_id: null, sort_order: 0, latitude: null, longitude: null, geo_source: 'unset', pool: null, prefixes: [], tags: [], tags_excluded: [], effective_tags: [] },
  { id: 'edge', name: 'Edge', group_type: 'generic', parent_id: 'tokyo', sort_order: 0, latitude: null, longitude: null, geo_source: 'unset', pool: null, prefixes: [], tags: [], tags_excluded: [], effective_tags: [] },
  { id: 'osaka', name: 'Osaka', group_type: 'site', parent_id: null, sort_order: 1, latitude: null, longitude: null, geo_source: 'unset', pool: null, prefixes: [], tags: [], tags_excluded: [], effective_tags: [] },
];

const node = (id: string, group_id: string | null): NodeSummary => ({
  id,
  name: id,
  address: '10.0.0.1',
  state: 'ok',
  vendor: null,
  model: null,
  group_id,
  sort_order: 0,
  kind: 'device',
});
const nodes: NodeSummary[] = [node('n1', 'tokyo'), node('n2', 'edge'), node('n3', 'osaka')];

/** The default expiry for every fixture below.
 *
 *  It has to be in the future, because the suppression helpers judge a stored expiry against the
 *  clock rather than trusting the payload's `active` flag — a tab left open past a window's end
 *  used to keep drawing its marker forever. Tests *about* that judgement pass `now` explicitly and
 *  say so; every other test just wants "still in force". */
const FUTURE = '2099-01-02T00:00:00Z';

const window = (over: Partial<MaintenanceWindow>): MaintenanceWindow => ({
  id: 'w',
  name: 'w',
  scope_level: 'node',
  scope_id: 'n1',
  starts_at: '2026-01-01T00:00:00Z',
  ends_at: FUTURE,
  enabled: true,
  active: true,
  ...over,
});

describe('groupSubtree', () => {
  it('includes the root and all descendants', () => {
    expect([...groupSubtree(groups, 'tokyo')].sort()).toEqual(['edge', 'tokyo']);
  });
  it('is just the root for a leaf group', () => {
    expect([...groupSubtree(groups, 'osaka')]).toEqual(['osaka']);
  });
});

describe('buildSuppressionIndex', () => {
  it('marks a node-scoped maintenance window on the node only', () => {
    const idx = buildSuppressionIndex([window({ scope_level: 'node', scope_id: 'n1' })], groups, nodes);
    expect(idx.maintenanceNodes.has('n1')).toBe(true);
    expect(idx.maintenanceNodes.has('n2')).toBe(false);
    expect(idx.maintenanceGroups.size).toBe(0);
  });

  it('propagates a folder-group maintenance window down the subtree (groups + member nodes)', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'group_id', scope_id: 'tokyo' })],
      groups,
      nodes,
    );
    expect([...idx.maintenanceGroups].sort()).toEqual(['edge', 'tokyo']);
    // n1 (tokyo) + n2 (edge subgroup) covered; n3 (osaka) not.
    expect(idx.maintenanceNodes.has('n1')).toBe(true);
    expect(idx.maintenanceNodes.has('n2')).toBe(true);
    expect(idx.maintenanceNodes.has('n3')).toBe(false);
  });

  it('ignores an inactive maintenance window', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'group_id', scope_id: 'tokyo', active: false })],
      groups,
      nodes,
    );
    expect(idx.maintenanceGroups.size).toBe(0);
    expect(idx.maintenanceNodes.size).toBe(0);
  });

  it('does not map profile windows onto tree rows (those surface via node state)', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'profile', scope_id: 'p1' })],
      groups,
      nodes,
    );
    expect(idx.maintenanceNodes.size).toBe(0);
    expect(idx.maintenanceGroups.size).toBe(0);
  });

});

const exemption = (over: Partial<SuppressionExemption>): SuppressionExemption => ({
  id: 'x',
  kind: 'maintenance',
  node_id: 'n2',
  until_at: FUTURE,
  ...over,
});

describe('groupContainers', () => {
  it('is the group itself plus every group above it', () => {
    expect(groupContainers(groups, 'edge')).toEqual(['edge', 'tokyo']);
    expect(groupContainers(groups, 'tokyo')).toEqual(['tokyo']);
    expect(groupContainers(groups, null)).toEqual([]);
  });

  // The mirror image of groupSubtree, and the property the release path relies on: the server
  // decides folder coverage by walking *down* from the window's group, the tree by walking *up*
  // from the row. They must agree, or the panel offers a release the server then refuses.
  it('agrees with groupSubtree in the other direction', () => {
    for (const root of ['tokyo', 'edge', 'osaka']) {
      for (const g of ['tokyo', 'edge', 'osaka']) {
        expect(groupContainers(groups, g).includes(root)).toBe(
          [...groupSubtree(groups, root)].includes(g),
        );
      }
    }
  });

  it('cannot loop forever on cyclic data', () => {
    const cyclic: NodeGroup[] = [
      { ...groups[0], id: 'a', parent_id: 'b' },
      { ...groups[0], id: 'b', parent_id: 'a' },
    ];
    expect(groupContainers(cyclic, 'a').sort()).toEqual(['a', 'b']);
  });
});

describe('buildSuppressionIndex with exemptions', () => {
  it('takes a released node out of the inherited marks but leaves its siblings', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'group_id', scope_id: 'tokyo' })],
      groups,
      nodes,
      [exemption({ node_id: 'n2' })],
    );
    expect(idx.maintenanceNodes.has('n2')).toBe(false);
    expect(idx.exemptMaintenanceNodes.has('n2')).toBe(true);
    expect(idx.maintenanceNodes.has('n1')).toBe(true);
    // The group itself is still under the window — one member being out does not release it.
    expect([...idx.maintenanceGroups].sort()).toEqual(['edge', 'tokyo']);
  });

  // The disagreement an operator would read as the icon lying: the engine keeps applying a window
  // that names the node, so the row must keep showing it.
  it('does not release a node from a window that names it', () => {
    const idx = buildSuppressionIndex(
      [
        window({ scope_level: 'group_id', scope_id: 'tokyo' }),
        window({ id: 'w2', scope_level: 'node', scope_id: 'n2' }),
      ],
      groups,
      nodes,
      [exemption({ node_id: 'n2' })],
    );
    expect(idx.maintenanceNodes.has('n2')).toBe(true);
  });

});

describe('causesFor', () => {
  const nodeTarget = { kind: 'node' as const, id: 'n2', name: 'n2' };
  const groupTarget = { kind: 'group' as const, id: 'edge', name: 'Edge' };

  it('calls a window that names the node direct, and carries its id so it can be ended', () => {
    const w = window({ id: 'w1', name: 'sw upgrade', scope_level: 'node', scope_id: 'n2' });
    const [cause] = causesFor(nodeTarget, { windows: [w], groups });
    expect(cause).toMatchObject({
      source: 'direct',
      id: 'w1',
      title: 'sw upgrade',
      labelKey: 'tree.suppression.from.node',
      endsAt: FUTURE,
    });
  });

  it('names the ancestor group an inherited window comes from, and offers no row to act on', () => {
    const w = window({ id: 'w1', scope_level: 'group_id', scope_id: 'tokyo' });
    const [cause] = causesFor(nodeTarget, {
      windows: [w],
      groups,
      node: node('n2', 'edge'),
    });
    expect(cause.source).toBe('inherited');
    expect(cause.id).toBeUndefined();
    expect(cause.labelKey).toBe('tree.suppression.from.ancestor');
    expect(cause.labelParams).toEqual({ name: 'Tokyo' });
  });

  it("reads a group's own window as direct and its parent's as inherited", () => {
    const own = window({ id: 'own', scope_level: 'group_id', scope_id: 'edge' });
    const above = window({ id: 'above', scope_level: 'group_id', scope_id: 'tokyo' });
    const causes = causesFor(groupTarget, { windows: [own, above], groups });
    expect(causes.map((c) => [c.id, c.source])).toEqual([
      ['own', 'direct'],
      [undefined, 'inherited'],
    ]);
  });

  // The honest fallback. A profile/tag/fleet-wide window is resolved server-side against facts the
  // tree never loads, so the state is all the browser has — saying "something else" beats both
  // inventing a cause and showing an empty panel under a lit icon.
  it('reports an unexplained maintenance state rather than an empty panel', () => {
    const causes = causesFor(nodeTarget, {
      windows: [],
      groups,
      node: { ...node('n2', 'edge'), state: 'maintenance' },
    });
    expect(causes).toHaveLength(1);
    expect(causes[0]).toMatchObject({
      source: 'unattributed',
      labelKey: 'tree.suppression.from.elsewhere',
      endsAt: '',
    });
  });

  it('does not add the unexplained cause when a window already accounts for the state', () => {
    const w = window({ scope_level: 'node', scope_id: 'n2' });
    const causes = causesFor(nodeTarget, {
      windows: [w],
      groups,
      node: { ...node('n2', 'edge'), state: 'maintenance' },
    });
    expect(causes.map((c) => c.source)).toEqual(['direct']);
  });

  it('ignores an inactive window and another branch of the tree', () => {
    const causes = causesFor(nodeTarget, {
      windows: [
        window({ id: 'off', scope_level: 'group_id', scope_id: 'tokyo', active: false }),
        window({ id: 'elsewhere', scope_level: 'group_id', scope_id: 'osaka' }),
      ],
      groups,
      node: node('n2', 'edge'),
    });
    expect(causes).toEqual([]);
  });

  // A key built at runtime is missing from *both* locales when forgotten, so parity passes and the
  // operator sees the raw key. This pins the produced set to the list `i18nEnumKeys` checks.
  it('only ever produces a key from CAUSE_LABEL_KEYS', () => {
    const causes = causesFor(nodeTarget, {
      windows: [
        window({ id: 'a', scope_level: 'node', scope_id: 'n2' }),
        window({ id: 'b', scope_level: 'group_id', scope_id: 'tokyo' }),
      ],
      groups,
      node: node('n2', 'edge'),
    });
    const groupCauses = causesFor(groupTarget, {
      windows: [window({ id: 'e', scope_level: 'group_id', scope_id: 'edge' })],
      groups,
    });
    const unexplained = causesFor(nodeTarget, {
      windows: [],
      groups,
      node: { ...node('n2', 'edge'), state: 'maintenance' },
    });
    const produced = new Set(
      [...causes, ...groupCauses, ...unexplained].map((c) => c.labelKey),
    );
    expect([...produced].sort()).toEqual([...CAUSE_LABEL_KEYS].sort());
  });
});

describe('suppressionPanelRows', () => {
  const nodeTarget = { kind: 'node' as const, id: 'n2', name: 'n2' };
  const groupTarget = { kind: 'group' as const, id: 'edge', name: 'Edge' };
  const groupWindow = window({ id: 'w1', name: 'DC1 planned', scope_level: 'group_id', scope_id: 'tokyo' });
  const n2 = node('n2', 'edge');

  it('offers to end a window that names the row', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [window({ id: 'w', scope_level: 'node', scope_id: 'n2' })],
      groups,
      node: n2,
    });
    expect(rows.map((r) => [r.headKey, r.action?.labelKey])).toEqual([
      ['tree.suppression.head.maintenance', 'tree.suppression.act.endNow'],
    ]);
    expect(rows[0].action?.action).toEqual({ action: 'end-window', windowId: 'w' });
  });

  // Exemptions of any other kind come only from a core older than ADR-234; they release nothing a
  // window covers, so the panel must neither honour them nor offer to undo them.
  it('ignores a release that is not from maintenance', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      groups,
      node: n2,
      exemptions: [{ ...exemption({ node_id: 'n2' }), kind: 'other' } as unknown as SuppressionExemption],
    });
    expect(rows.map((r) => r.headKey)).toEqual(['tree.suppression.head.maintenance']);
  });

  it('offers a node its own way out of coverage it only inherits', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      groups,
      node: n2,
    });
    expect(rows[0].action).toEqual({
      action: { action: 'release-node', nodeId: 'n2' },
      labelKey: 'tree.suppression.act.excludeNode',
    });
  });

  // The bug this function exists to make untestable-in-a-.tsx: the panel used to show a live
  // "in maintenance" block offering a release the node had already been given, directly above the
  // block saying it was released. One block, one action, and the action is the undo.
  it('shows a released node one block, and the only action is putting it back', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].headKey).toBe('tree.suppression.head.releasedMaintenance');
    expect(rows[0].action).toEqual({
      action: { action: 'undo-release', nodeId: 'n2' },
      labelKey: 'tree.suppression.act.undoMaintenance',
    });
  });

  // Being released is not the same as there being nothing to be released from: the operator needs
  // to see which window they are standing outside of, and until when.
  it('keeps the window a release was carved out of on screen', () => {
    const [row] = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
    });
    expect(row.title).toBe('DC1 planned');
    expect(row.labelKey).toBe('tree.suppression.from.ancestor');
    expect(row.labelParams).toEqual({ name: 'Tokyo' });
    expect(row.endsAt).toBe(FUTURE);
  });

  // An exemption cancels inherited coverage only, so a window aimed at the node still suppresses
  // it. Both facts are true at once and the panel says both, in that order.
  it('still reports a window that names a released node as suppressing it', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [window({ id: 'own', scope_level: 'node', scope_id: 'n2' }), groupWindow],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
    });
    expect(rows.map((r) => [r.headKey, r.action?.labelKey])).toEqual([
      ['tree.suppression.head.maintenance', 'tree.suppression.act.endNow'],
      ['tree.suppression.head.releasedMaintenance', 'tree.suppression.act.undoMaintenance'],
    ]);
  });

  // A release from coverage the browser cannot attribute — a profile, tag or fleet-wide window,
  // resolved server-side against facts this page never loads. Nothing explains the block, so it
  // has no context; the undo must still be reachable, or the node keeps a release nobody can see
  // or cancel. (A release whose coverage has *ended* no longer reaches here: the server re-derives
  // every expiry against what is in force, and the clock filter hides whatever is left.)
  it('can still undo a release nothing explains', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance', until_at: FUTURE })],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].headKey).toBe('tree.suppression.head.releasedMaintenance');
    expect(rows[0].labelKey).toBeUndefined();
    expect(rows[0].endsAt).toBe(FUTURE);
  });

  it('gives a group covered from above a note instead of a button', () => {
    const [row] = suppressionPanelRows(groupTarget, {
      windows: [groupWindow],
      groups,
    });
    expect(row.action).toBeNull();
    expect(row.noteKey).toBe('tree.suppression.releaseOnAncestor');
    expect(row.noteParams).toEqual({ name: 'Tokyo' });
  });

  it('is empty when nothing suppresses the row', () => {
    expect(suppressionPanelRows(nodeTarget, { windows: [], groups, node: n2 })).toEqual(
      [],
    );
  });

  // Same guard as the cause labels, one level up: every heading, button and note the panel renders
  // is assembled from a value at runtime, so a forgotten string is missing from both locales and
  // renders as a raw key.
  it('only ever produces a key from PANEL_LABEL_KEYS', () => {
    const cases: SuppressionPanelRow[][] = [
      suppressionPanelRows(nodeTarget, {
        windows: [window({ id: 'own', scope_level: 'node', scope_id: 'n2' }), groupWindow],
        groups,
        node: n2,
      }),
      suppressionPanelRows(nodeTarget, {
        windows: [groupWindow],
        groups,
        node: n2,
        exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
      }),
      suppressionPanelRows(groupTarget, { windows: [groupWindow], groups }),
    ];
    const produced = new Set(
      cases
        .flat()
        .flatMap((r) => [r.headKey, r.action?.labelKey, r.noteKey])
        .filter((k): k is string => !!k),
    );
    expect([...produced].sort()).toEqual([...PANEL_LABEL_KEYS].sort());
  });
});

// The page holds these lists for as long as the tab is open and only refetched them after an
// action, so a window that ended went on marking its rows for hours. The server's `active` flag is
// a fact about when the payload was built; the clock is the fact about now.
describe('suppression expires on the clock, not on the payload', () => {
  const T0 = Date.parse('2026-08-12T06:00:00Z');
  const ended = '2026-08-12T05:00:00Z';

  it('drops a window whose end has passed, even though the payload still says active', () => {
    const w = window({ scope_level: 'group_id', scope_id: 'tokyo', ends_at: ended, active: true });
    const idx = buildSuppressionIndex([w], groups, nodes, [], T0);
    expect(idx.maintenanceGroups.size).toBe(0);
    expect(idx.maintenanceNodes.size).toBe(0);
    // Still in force one hour earlier — it is the clock doing this, not the window being ignored.
    const before = buildSuppressionIndex([w], groups, nodes, [], Date.parse(ended) - 1);
    expect(before.maintenanceNodes.has('n2')).toBe(true);
  });

  it('drops an expired release', () => {
    const idx = buildSuppressionIndex(
      [],
      groups,
      nodes,
      [exemption({ node_id: 'n2', until_at: ended })],
      T0,
    );
    expect(idx.exemptMaintenanceNodes.size).toBe(0);
  });

  it('keeps a marker whose expiry cannot be parsed', () => {
    // Failing toward "still suppressed": hiding a marker tells an operator alerts will reach them
    // when they will not, which is the worse of the two mistakes.
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'group_id', scope_id: 'tokyo', ends_at: 'not a date' })],
      groups,
      nodes,
      [],
      T0,
    );
    expect(idx.maintenanceNodes.has('n2')).toBe(true);
  });

  it('says nothing is suppressing a row whose window is over', () => {
    const rows = suppressionPanelRows(
      { kind: 'node', id: 'n2', name: 'n2' },
      {
        windows: [window({ scope_level: 'group_id', scope_id: 'tokyo', ends_at: ended })],
          groups,
        node: node('n2', 'edge'),
        now: T0,
      },
    );
    expect(rows).toEqual([]);
  });

  it('does not offer to put a node back under a release that has run out', () => {
    const rows = suppressionPanelRows(
      { kind: 'node', id: 'n2', name: 'n2' },
      {
        windows: [],
          groups,
        node: node('n2', 'edge'),
        exemptions: [exemption({ node_id: 'n2', until_at: ended })],
        now: T0,
      },
    );
    expect(rows).toEqual([]);
  });
});

describe('nextSuppressionExpiry', () => {
  const T0 = Date.parse('2026-08-12T06:00:00Z');
  const at = (iso: string) => Date.parse(iso);

  it('is the earliest expiry still ahead, across both lists', () => {
    expect(
      nextSuppressionExpiry(
        [window({ ends_at: '2026-08-12T09:00:00Z' })],
        [exemption({ until_at: '2026-08-12T08:00:00Z' })],
        T0,
      ),
    ).toBe(at('2026-08-12T08:00:00Z'));
  });

  it('ignores what is already past, so a clock the server disagrees with cannot start a refetch loop', () => {
    expect(
      nextSuppressionExpiry(
        [window({ ends_at: '2026-08-12T05:00:00Z' })],
        [exemption({ until_at: '2026-08-12T05:59:00Z' })],
        T0,
      ),
    ).toBeNull();
  });

  it('ignores an inactive window — it is not marking anything to begin with', () => {
    expect(
      nextSuppressionExpiry([window({ active: false, ends_at: '2026-08-12T07:00:00Z' })], [], T0),
    ).toBeNull();
  });

  it('is null when there is nothing to wait for', () => {
    expect(nextSuppressionExpiry([], [], T0)).toBeNull();
  });
});

describe('suppressionRefreshDelayMs', () => {
  const T0 = Date.parse('2026-08-12T06:00:00Z');
  const DAY = 86_400_000;

  it('waits until a second past a near expiry — the cap changes nothing it does not have to', () => {
    expect(suppressionRefreshDelayMs(T0 + 90 * 60_000, T0)).toBe(90 * 60_000 + 1000);
    // Just under the ceiling is still exact, so the cap is a ceiling and not a replacement.
    expect(suppressionRefreshDelayMs(T0 + 24 * DAY, T0)).toBe(24 * DAY + 1000);
  });

  it('never exceeds what setTimeout can hold, or the timer fires at once and re-arms forever', () => {
    // A quarter-long window: 90 days is 7.776e9 ms, which as a signed 32-bit integer is not 90 days.
    const delay = suppressionRefreshDelayMs(T0 + 90 * DAY, T0);
    expect(delay).toBe(MAX_TIMER_DELAY_MS);
    expect(delay).toBeLessThanOrEqual(2 ** 31 - 1);
    // The value a browser would actually use must be the value we asked for.
    expect(delay | 0).toBe(delay);
  });

  it('is never negative when the expiry has just passed', () => {
    expect(suppressionRefreshDelayMs(T0 - 5, T0)).toBe(1000);
  });
});
