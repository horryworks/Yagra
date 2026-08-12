// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type {
  Alert,
  MaintenanceWindow,
  Mute,
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
  muteTargetFromAlert,
  PANEL_LABEL_KEYS,
  suppressionPanelRows,
  type SuppressionPanelRow,
} from './suppression';
import { LIVENESS_METRIC } from './format';

// Tree: tokyo ⊃ edge ⊃ (n2); tokyo also holds n1. osaka is a separate top-level group with n3.
const groups: NodeGroup[] = [
  { id: 'tokyo', name: 'Tokyo', group_type: 'site', parent_id: null, sort_order: 0, latitude: null, longitude: null, geo_source: 'unset', pool: null },
  { id: 'edge', name: 'Edge', group_type: 'generic', parent_id: 'tokyo', sort_order: 0, latitude: null, longitude: null, geo_source: 'unset', pool: null },
  { id: 'osaka', name: 'Osaka', group_type: 'site', parent_id: null, sort_order: 1, latitude: null, longitude: null, geo_source: 'unset', pool: null },
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

const window = (over: Partial<MaintenanceWindow>): MaintenanceWindow => ({
  id: 'w',
  name: 'w',
  scope_level: 'node',
  scope_id: 'n1',
  starts_at: '2026-01-01T00:00:00Z',
  ends_at: '2026-01-02T00:00:00Z',
  enabled: true,
  active: true,
  ...over,
});

const mute = (over: Partial<Mute>): Mute => ({
  id: 'm',
  scope_kind: 'node',
  node_id: 'n1',
  group_id: null,
  metric_name: null,
  until_at: '2026-01-02T00:00:00Z',
  reason: null,
  ...over,
});

const alert = (over: Partial<Alert>): Alert => ({
  node: 'n1',
  subject_kind: 'node',
  check: 'c1',
  metric: 'icmp_rtt_ms',
  severity: 'critical',
  state: 'critical',
  flapping: false,
  at_unix_ms: 0,
  ...over,
});

describe('muteTargetFromAlert', () => {
  it('targets the node it is given, named for display', () => {
    // The node is a parameter, not `alert.node`: that field is the alert's *subject* and is only
    // a node id when the subject is one. The caller resolves it via `lib/alertSubject`.
    const seed = muteTargetFromAlert(alert({ node: 'n2' }), 'n2', 'core-sw-01');
    expect(seed.target).toEqual({ kind: 'node', id: 'n2', name: 'core-sw-01' });
  });

  it('pre-fills the metric so the mute matches exactly the check that fired', () => {
    expect(muteTargetFromAlert(alert({ metric: 'huawei_cpu_usage' }), 'n1', 'n').metric).toBe(
      'huawei_cpu_usage',
    );
  });

  // Load-bearing: the backend re-derives a mute's identity as check_id(node, name), the same v5
  // hash the alert's `check` is. Substituting the display text ("Reachability") — or dropping the
  // sentinel and falling back to a whole-node mute — would silence something other than what the
  // operator clicked. The form renders it readably; the seed must stay verbatim.
  it('carries the liveness sentinel through verbatim, not its display text', () => {
    expect(muteTargetFromAlert(alert({ metric: LIVENESS_METRIC }), 'n1', 'n').metric).toBe(
      LIVENESS_METRIC,
    );
  });

  it('seeds a whole-node mute when the alert captured no metric (pre-migration-0036 row)', () => {
    expect(muteTargetFromAlert(alert({ metric: '' }), 'n1', 'n').metric).toBeUndefined();
  });
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
    const idx = buildSuppressionIndex([window({ scope_level: 'node', scope_id: 'n1' })], [], groups, nodes);
    expect(idx.maintenanceNodes.has('n1')).toBe(true);
    expect(idx.maintenanceNodes.has('n2')).toBe(false);
    expect(idx.maintenanceGroups.size).toBe(0);
  });

  it('propagates a folder-group maintenance window down the subtree (groups + member nodes)', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'group_id', scope_id: 'tokyo' })],
      [],
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
      [],
      groups,
      nodes,
    );
    expect(idx.maintenanceGroups.size).toBe(0);
    expect(idx.maintenanceNodes.size).toBe(0);
  });

  it('does not map profile/tag windows onto tree rows (those surface via node state)', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'profile', scope_id: 'p1' }), window({ scope_level: 'group', scope_id: 'tokyo' })],
      [],
      groups,
      nodes,
    );
    expect(idx.maintenanceNodes.size).toBe(0);
    expect(idx.maintenanceGroups.size).toBe(0);
  });

  it('marks a node mute on the node, and a group mute down the subtree', () => {
    const idx = buildSuppressionIndex(
      [],
      [
        mute({ scope_kind: 'node', node_id: 'n3', group_id: null }),
        mute({ id: 'm2', scope_kind: 'group', node_id: null, group_id: 'tokyo' }),
      ],
      groups,
      nodes,
    );
    expect(idx.muteNodes.has('n3')).toBe(true);
    expect([...idx.muteGroups].sort()).toEqual(['edge', 'tokyo']);
    expect(idx.muteNodes.has('n1')).toBe(true);
    expect(idx.muteNodes.has('n2')).toBe(true);
  });
});

const exemption = (over: Partial<SuppressionExemption>): SuppressionExemption => ({
  id: 'x',
  kind: 'maintenance',
  node_id: 'n2',
  until_at: '2026-01-02T00:00:00Z',
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
      [],
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
      [],
      groups,
      nodes,
      [exemption({ node_id: 'n2' })],
    );
    expect(idx.maintenanceNodes.has('n2')).toBe(true);
  });

  it('keeps the two kinds apart', () => {
    const idx = buildSuppressionIndex(
      [window({ scope_level: 'group_id', scope_id: 'tokyo' })],
      [mute({ id: 'm2', scope_kind: 'group', node_id: null, group_id: 'tokyo' })],
      groups,
      nodes,
      [exemption({ node_id: 'n2', kind: 'mute' })],
    );
    // Released from the group mute only — still in the group's maintenance window.
    expect(idx.muteNodes.has('n2')).toBe(false);
    expect(idx.maintenanceNodes.has('n2')).toBe(true);
    expect(idx.exemptMuteNodes.has('n2')).toBe(true);
    expect(idx.exemptMaintenanceNodes.size).toBe(0);
  });

  it('a node mute still applies to a node released from its group mute', () => {
    const idx = buildSuppressionIndex(
      [],
      [
        mute({ id: 'm2', scope_kind: 'group', node_id: null, group_id: 'tokyo' }),
        mute({ id: 'm3', scope_kind: 'node', node_id: 'n2', group_id: null }),
      ],
      groups,
      nodes,
      [exemption({ node_id: 'n2', kind: 'mute' })],
    );
    expect(idx.muteNodes.has('n2')).toBe(true);
  });
});

describe('causesFor', () => {
  const nodeTarget = { kind: 'node' as const, id: 'n2', name: 'n2' };
  const groupTarget = { kind: 'group' as const, id: 'edge', name: 'Edge' };

  it('calls a window that names the node direct, and carries its id so it can be ended', () => {
    const w = window({ id: 'w1', name: 'sw upgrade', scope_level: 'node', scope_id: 'n2' });
    const [cause] = causesFor(nodeTarget, { windows: [w], mutes: [], groups });
    expect(cause).toMatchObject({
      kind: 'maintenance',
      source: 'direct',
      id: 'w1',
      title: 'sw upgrade',
      labelKey: 'tree.suppression.from.node',
      endsAt: '2026-01-02T00:00:00Z',
    });
  });

  it('names the ancestor group an inherited window comes from, and offers no row to act on', () => {
    const w = window({ id: 'w1', scope_level: 'group_id', scope_id: 'tokyo' });
    const [cause] = causesFor(nodeTarget, {
      windows: [w],
      mutes: [],
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
    const causes = causesFor(groupTarget, { windows: [own, above], mutes: [], groups });
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
      mutes: [],
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
      mutes: [],
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
      mutes: [mute({ id: 'm2', scope_kind: 'group', node_id: null, group_id: 'osaka' })],
      groups,
      node: node('n2', 'edge'),
    });
    expect(causes).toEqual([]);
  });

  it('separates a node mute from an inherited group mute', () => {
    const causes = causesFor(nodeTarget, {
      windows: [],
      mutes: [
        mute({ id: 'own', scope_kind: 'node', node_id: 'n2', metric_name: 'icmp_rtt_ms' }),
        mute({ id: 'up', scope_kind: 'group', node_id: null, group_id: 'tokyo' }),
      ],
      groups,
      node: node('n2', 'edge'),
    });
    expect(causes.map((c) => [c.source, c.id])).toEqual([
      ['direct', 'own'],
      ['inherited', undefined],
    ]);
    expect(causes[0].title).toBe('icmp_rtt_ms');
  });

  // A key built at runtime is missing from *both* locales when forgotten, so parity passes and the
  // operator sees the raw key. This pins the produced set to the list `i18nEnumKeys` checks.
  it('only ever produces a key from CAUSE_LABEL_KEYS', () => {
    const causes = causesFor(nodeTarget, {
      windows: [
        window({ id: 'a', scope_level: 'node', scope_id: 'n2' }),
        window({ id: 'b', scope_level: 'group_id', scope_id: 'tokyo' }),
      ],
      mutes: [
        mute({ id: 'c', scope_kind: 'node', node_id: 'n2' }),
        mute({ id: 'd', scope_kind: 'group', node_id: null, group_id: 'tokyo' }),
      ],
      groups,
      node: node('n2', 'edge'),
    });
    const groupCauses = causesFor(groupTarget, {
      windows: [window({ id: 'e', scope_level: 'group_id', scope_id: 'edge' })],
      mutes: [],
      groups,
    });
    const unexplained = causesFor(nodeTarget, {
      windows: [],
      mutes: [],
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

  it('offers to end a window that names the row, and to lift a mute that does', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [window({ id: 'w', scope_level: 'node', scope_id: 'n2' })],
      mutes: [mute({ id: 'm', scope_kind: 'node', node_id: 'n2' })],
      groups,
      node: n2,
    });
    expect(rows.map((r) => [r.headKey, r.action?.labelKey])).toEqual([
      ['tree.suppression.head.maintenance', 'tree.suppression.act.endNow'],
      ['tree.suppression.head.mute', 'tree.suppression.act.unmute'],
    ]);
    expect(rows[0].action?.action).toEqual({ action: 'end-window', windowId: 'w' });
    expect(rows[1].action?.action).toEqual({ action: 'lift-mute', muteId: 'm' });
  });

  it('offers a node its own way out of coverage it only inherits', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      mutes: [],
      groups,
      node: n2,
    });
    expect(rows[0].action).toEqual({
      action: { action: 'release-node', nodeId: 'n2', kind: 'maintenance' },
      labelKey: 'tree.suppression.act.excludeNode',
    });
  });

  // The bug this function exists to make untestable-in-a-.tsx: the panel used to show a live
  // "in maintenance" block offering a release the node had already been given, directly above the
  // block saying it was released. One block, one action, and the action is the undo.
  it('shows a released node one block, and the only action is putting it back', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      mutes: [],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].headKey).toBe('tree.suppression.head.releasedMaintenance');
    expect(rows[0].action).toEqual({
      action: { action: 'undo-release', nodeId: 'n2', kind: 'maintenance' },
      labelKey: 'tree.suppression.act.undoMaintenance',
    });
  });

  // Being released is not the same as there being nothing to be released from: the operator needs
  // to see which window they are standing outside of, and until when.
  it('keeps the window a release was carved out of on screen', () => {
    const [row] = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      mutes: [],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
    });
    expect(row.title).toBe('DC1 planned');
    expect(row.labelKey).toBe('tree.suppression.from.ancestor');
    expect(row.labelParams).toEqual({ name: 'Tokyo' });
    expect(row.endsAt).toBe('2026-01-02T00:00:00Z');
  });

  // An exemption cancels inherited coverage only, so a window aimed at the node still suppresses
  // it. Both facts are true at once and the panel says both, in that order.
  it('still reports a window that names a released node as suppressing it', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [window({ id: 'own', scope_level: 'node', scope_id: 'n2' }), groupWindow],
      mutes: [],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance' })],
    });
    expect(rows.map((r) => [r.headKey, r.action?.labelKey])).toEqual([
      ['tree.suppression.head.maintenance', 'tree.suppression.act.endNow'],
      ['tree.suppression.head.releasedMaintenance', 'tree.suppression.act.undoMaintenance'],
    ]);
  });

  it('releases the two kinds independently', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [groupWindow],
      mutes: [mute({ id: 'gm', scope_kind: 'group', node_id: null, group_id: 'tokyo' })],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'mute' })],
    });
    expect(rows.map((r) => [r.kind, r.headKey, r.action?.labelKey])).toEqual([
      ['maintenance', 'tree.suppression.head.maintenance', 'tree.suppression.act.excludeNode'],
      ['mute', 'tree.suppression.head.releasedMute', 'tree.suppression.act.undoMute'],
    ]);
  });

  // The covering window ended before the exemption row was swept. Nothing explains the release, so
  // the block has no context — but the undo must still be reachable, or the node keeps a release
  // nobody can see or cancel.
  it('can still undo a release nothing explains', () => {
    const rows = suppressionPanelRows(nodeTarget, {
      windows: [],
      mutes: [],
      groups,
      node: n2,
      exemptions: [exemption({ node_id: 'n2', kind: 'maintenance', until_at: '2026-03-01T00:00:00Z' })],
    });
    expect(rows).toHaveLength(1);
    expect(rows[0].headKey).toBe('tree.suppression.head.releasedMaintenance');
    expect(rows[0].labelKey).toBeUndefined();
    expect(rows[0].endsAt).toBe('2026-03-01T00:00:00Z');
  });

  it('gives a group covered from above a note instead of a button', () => {
    const [row] = suppressionPanelRows(groupTarget, {
      windows: [groupWindow],
      mutes: [],
      groups,
    });
    expect(row.action).toBeNull();
    expect(row.noteKey).toBe('tree.suppression.releaseOnAncestor');
    expect(row.noteParams).toEqual({ name: 'Tokyo' });
  });

  it('is empty when nothing suppresses the row', () => {
    expect(suppressionPanelRows(nodeTarget, { windows: [], mutes: [], groups, node: n2 })).toEqual(
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
        mutes: [
          mute({ id: 'own', scope_kind: 'node', node_id: 'n2' }),
          mute({ id: 'gm', scope_kind: 'group', node_id: null, group_id: 'tokyo' }),
        ],
        groups,
        node: n2,
      }),
      suppressionPanelRows(nodeTarget, {
        windows: [groupWindow],
        mutes: [mute({ id: 'gm', scope_kind: 'group', node_id: null, group_id: 'tokyo' })],
        groups,
        node: n2,
        exemptions: [
          exemption({ node_id: 'n2', kind: 'maintenance' }),
          exemption({ id: 'x2', node_id: 'n2', kind: 'mute' }),
        ],
      }),
      suppressionPanelRows(groupTarget, { windows: [groupWindow], mutes: [], groups }),
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
