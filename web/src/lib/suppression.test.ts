// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { MaintenanceWindow, Mute, NodeGroup, NodeSummary } from '../types/api';
import { buildSuppressionIndex, groupSubtree } from './suppression';

// Tree: tokyo ⊃ edge ⊃ (n2); tokyo also holds n1. osaka is a separate top-level group with n3.
const groups: NodeGroup[] = [
  { id: 'tokyo', name: 'Tokyo', group_type: 'site', parent_id: null, sort_order: 0, latitude: null, longitude: null },
  { id: 'edge', name: 'Edge', group_type: 'generic', parent_id: 'tokyo', sort_order: 0, latitude: null, longitude: null },
  { id: 'osaka', name: 'Osaka', group_type: 'site', parent_id: null, sort_order: 1, latitude: null, longitude: null },
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
