// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { addMenuTarget, type AddMenuTarget } from './nodesAddMenu';
import type { NodeGroup, NodeSummary } from '../types/api';

const group = (id: string, name: string, parent: string | null = null): NodeGroup => ({
  id,
  name,
  group_type: 'generic',
  parent_id: parent,
  sort_order: 0,
  latitude: null,
  longitude: null,
  geo_source: 'unset',
  pool: null,
});

const node = (id: string, name: string, groupId: string | null): NodeSummary => ({
  id,
  name,
  address: '10.0.0.1',
  state: 'ok',
  vendor: null,
  model: null,
  group_id: groupId,
  sort_order: 0,
  kind: 'device',
});

const GROUPS = [group('g1', 'Tokyo'), group('g2', 'Rack A', 'g1')];
const NODES = [node('n1', 'sw01', 'g2'), node('n2', 'fw01', null), node('n3', 'ghost', 'gone')];

const TOP_LEVEL: AddMenuTarget = {
  groupId: null,
  groupName: null,
  addNodeKey: 'tree.addNodeEllipsis',
  addGroupKey: 'tree.addGroupEllipsis',
};

describe('addMenuTarget', () => {
  it('falls back to top level when nothing is selected', () => {
    expect(addMenuTarget(null, GROUPS, NODES)).toEqual(TOP_LEVEL);
  });

  it('targets the selected group and names it', () => {
    expect(addMenuTarget({ kind: 'group', id: 'g1' }, GROUPS, NODES)).toEqual({
      groupId: 'g1',
      groupName: 'Tokyo',
      addNodeKey: 'addMenu.nodeIn',
      addGroupKey: 'addMenu.groupIn',
    });
  });

  it("targets a selected node's own group", () => {
    expect(addMenuTarget({ kind: 'node', id: 'n1' }, GROUPS, NODES)).toEqual({
      groupId: 'g2',
      groupName: 'Rack A',
      addNodeKey: 'addMenu.nodeIn',
      addGroupKey: 'addMenu.groupIn',
    });
  });

  it('falls back to top level for an ungrouped node', () => {
    expect(addMenuTarget({ kind: 'node', id: 'n2' }, GROUPS, NODES)).toEqual(TOP_LEVEL);
  });

  // The tree loads members per group (A-3), and filter mode replaces them with a capped server
  // search page — so a selected node is not always in `nodes`. Guessing a folder would file the
  // new node somewhere the operator never named.
  it('falls back to top level when the selected node is not loaded', () => {
    expect(addMenuTarget({ kind: 'node', id: 'not-loaded' }, GROUPS, NODES)).toEqual(TOP_LEVEL);
  });

  // `?sel=group:<id>` survives a delete until the page's cleanup effect runs.
  it('falls back to top level when the selection points at a deleted group', () => {
    expect(addMenuTarget({ kind: 'group', id: 'gone' }, GROUPS, NODES)).toEqual(TOP_LEVEL);
  });

  it("falls back to top level when a node's group is not among the visible groups", () => {
    expect(addMenuTarget({ kind: 'node', id: 'n3' }, GROUPS, NODES)).toEqual(TOP_LEVEL);
  });

  it('never returns an unnamed target or a mixed key pair', () => {
    const selections = [
      null,
      { kind: 'group', id: 'g1' },
      { kind: 'group', id: 'g2' },
      { kind: 'group', id: 'gone' },
      { kind: 'node', id: 'n1' },
      { kind: 'node', id: 'n2' },
      { kind: 'node', id: 'n3' },
      { kind: 'node', id: 'not-loaded' },
    ] as const;
    for (const sel of selections) {
      const t = addMenuTarget(sel, GROUPS, NODES);
      const named = (t.groupId === null) === (t.groupName === null);
      const paired =
        (t.addNodeKey === 'addMenu.nodeIn') === (t.addGroupKey === 'addMenu.groupIn');
      expect({ sel, named, paired }).toEqual({ sel, named: true, paired: true });
    }
  });
});
