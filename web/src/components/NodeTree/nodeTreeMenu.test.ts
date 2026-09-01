// SPDX-License-Identifier: AGPL-3.0-only
// The rule that once shipped an operator a tree with no context menu at all — now somewhere a test
// can run it. The regression case is the first one below.
import { describe, expect, it } from 'vitest';
import {
  groupMenuHasItems,
  hasSuppression,
  nodeMenuHasItems,
  rootMenuHasItems,
  type MenuCapabilities,
} from './nodeTreeMenu';
import type { SuppressionIndex, SuppressionTarget } from '../../lib/suppression';
import type { NodeSummary } from '../../types/api';

const caps = (over: Partial<MenuCapabilities> = {}): MenuCapabilities => ({
  canEdit: false,
  canSuppress: false,
  canAddNode: false,
  ...over,
});

describe('which right-click menus have anything in them', () => {
  it('opens a group menu for an operator who may only suppress', () => {
    // 🚨 THE REGRESSION. `canEdit` is `ManageConfig`; maintenance and mute are `ManageMaintenance`
    // and `AckAlerts`, which an operator holds. Gating the whole menu on `canEdit` removed it
    // entirely for every operator — and read as deliberate, because an admin still saw it.
    expect(groupMenuHasItems(caps({ canSuppress: true }))).toBe(true);
    expect(groupMenuHasItems(caps({ canAddNode: true }))).toBe(true);
    expect(groupMenuHasItems(caps({ canEdit: true }))).toBe(true);
  });

  it('withholds a group menu only when every item is gone', () => {
    // An empty menu is worse than none: it opens, says nothing, and has to be dismissed.
    expect(groupMenuHasItems(caps())).toBe(false);
  });

  it('gates the root menu on its one item', () => {
    expect(rootMenuHasItems(caps({ canAddNode: true }))).toBe(true);
    // Nothing else can appear there, so no other permission may open it.
    expect(rootMenuHasItems(caps({ canEdit: true, canSuppress: true }))).toBe(false);
  });

  it('always opens a node menu, because Open needs no permission', () => {
    expect(nodeMenuHasItems()).toBe(true);
  });
});

describe('hasSuppression', () => {
  const idx = (over: Partial<Record<keyof SuppressionIndex, Set<string>>> = {}) =>
    ({
      maintenanceNodes: new Set<string>(),
      muteNodes: new Set<string>(),
      maintenanceGroups: new Set<string>(),
      muteGroups: new Set<string>(),
      exemptMaintenanceNodes: new Set<string>(),
      exemptMuteNodes: new Set<string>(),
      ...over,
    }) as SuppressionIndex;

  const node: SuppressionTarget = { kind: 'node', id: 'n1', name: 'edge-1' };
  const group: SuppressionTarget = { kind: 'group', id: 'g1', name: 'site' };

  it('is false for a row with nothing on it', () => {
    expect(hasSuppression(idx(), node)).toBe(false);
    expect(hasSuppression(idx(), group)).toBe(false);
  });

  it('is false when the page has no index at all', () => {
    // The index is fetched separately; the tree renders before it arrives.
    expect(hasSuppression(undefined, node)).toBe(false);
    expect(hasSuppression(undefined, group)).toBe(false);
  });

  it('counts a window or a mute, on either kind of row', () => {
    expect(hasSuppression(idx({ maintenanceNodes: new Set(['n1']) }), node)).toBe(true);
    expect(hasSuppression(idx({ muteNodes: new Set(['n1']) }), node)).toBe(true);
    expect(hasSuppression(idx({ maintenanceGroups: new Set(['g1']) }), group)).toBe(true);
    expect(hasSuppression(idx({ muteGroups: new Set(['g1']) }), group)).toBe(true);
  });

  it('counts an EXEMPT node too — a release still has to be explainable', () => {
    // "Why did this stop being silent" is the same question as "why is it silent", and the panel is
    // the only place either is answered. Dropping the exempt sets would make the released marker
    // open an empty panel.
    expect(hasSuppression(idx({ exemptMaintenanceNodes: new Set(['n1']) }), node)).toBe(true);
    expect(hasSuppression(idx({ exemptMuteNodes: new Set(['n1']) }), node)).toBe(true);
  });

  it('accepts the engine’s rolled-up state as one more reason, for a node only', () => {
    const inMaint = { state: 'maintenance' } as NodeSummary;
    expect(hasSuppression(idx(), node, inMaint)).toBe(true);
    // A group has no state of its own, so passing one must not change the group answer.
    expect(hasSuppression(idx(), group, inMaint)).toBe(false);
  });

  it('does not confuse a group id with a node id', () => {
    // Both sets are keyed by uuid and the two id spaces are different tables.
    expect(hasSuppression(idx({ maintenanceGroups: new Set(['n1']) }), node)).toBe(false);
    expect(hasSuppression(idx({ maintenanceNodes: new Set(['g1']) }), group)).toBe(false);
  });
});
