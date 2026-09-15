// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { DuplicateGroup, DuplicateMember, DuplicateNodesView } from '../types/api';
import {
  DELETE_MAX,
  confidenceCounts,
  deleteBlock,
  deleteTargets,
  emptyState,
  evidenceSummary,
  flattenRows,
  groupsLeftEmpty,
  ignoredShown,
  pruneSelection,
  selectAllButKeepers,
} from './duplicateNodes';

const member = (id: string, over: Partial<DuplicateMember> = {}): DuplicateMember => ({
  node_id: id,
  node_name: `node-${id}`,
  address: '10.0.0.1',
  group_id: null,
  vendor: null,
  model: null,
  serial_number: null,
  created_at: '2026-09-15T00:00:00Z',
  dependents: 0,
  suggested_keep: false,
  ...over,
});

const group = (members: DuplicateMember[], over: Partial<DuplicateGroup> = {}): DuplicateGroup => ({
  confidence: 'confident',
  evidence: [{ kind: 'address', value: '10.0.0.1', node_ids: members.map((m) => m.node_id) }],
  contradictions: [],
  members,
  ...over,
});

const view = (groups: DuplicateGroup[], over: Partial<DuplicateNodesView> = {}): DuplicateNodesView => ({
  groups,
  total: groups.length,
  ignored: [],
  ignored_total: 0,
  scanned: 10,
  with_serial: 4,
  with_address_list: 4,
  ...over,
});

/** Two groups: a1 (keeper) + a2, and b1 (keeper) + b2 + b3. */
const twoGroups = () =>
  view([
    group([member('a1', { suggested_keep: true }), member('a2')]),
    group([member('b1', { suggested_keep: true }), member('b2'), member('b3')], {
      confidence: 'possible',
    }),
  ]);

describe('flattenRows', () => {
  it('puts every member on its own row, numbers the groups and shades them alternately', () => {
    const rows = flattenRows(twoGroups());
    expect(rows.map((r) => [r.member.node_id, r.groupNumber, r.first, r.shade])).toEqual([
      ['a1', 1, true, 'even'],
      ['a2', 1, false, 'even'],
      ['b1', 2, true, 'odd'],
      ['b2', 2, false, 'odd'],
      ['b3', 2, false, 'odd'],
    ]);
  });

  it('is empty before the first read', () => {
    expect(flattenRows(null)).toEqual([]);
  });
});

describe('confidenceCounts', () => {
  it('counts the listed groups by confidence', () => {
    expect(confidenceCounts(twoGroups())).toEqual({ confident: 1, possible: 1 });
    expect(confidenceCounts(null)).toEqual({ confident: 0, possible: 0 });
  });
});

describe('selection', () => {
  it('selects every member except each group’s suggested keeper', () => {
    expect([...selectAllButKeepers(twoGroups())].sort()).toEqual(['a2', 'b2', 'b3']);
  });

  it('never leaves a group empty when the keepers are left out', () => {
    const v = twoGroups();
    expect(groupsLeftEmpty(v, selectAllButKeepers(v))).toEqual([]);
  });

  it('drops a selected node that is no longer listed after a reload', () => {
    const after = view([group([member('a1', { suggested_keep: true }), member('a2')])]);
    expect([...pruneSelection(new Set(['a2', 'b2', 'gone']), after)]).toEqual(['a2']);
  });
});

describe('deleteBlock', () => {
  it('refuses a delete that would take every member of a group, naming each such group', () => {
    const v = twoGroups();
    expect(deleteBlock(v, new Set(['a1', 'a2', 'b2']))).toEqual({
      key: 'duplicates.block.keepOne',
      groups: [1],
    });
    expect(groupsLeftEmpty(v, new Set(['a1', 'a2', 'b1', 'b2', 'b3']))).toEqual([1, 2]);
  });

  it('allows a delete that leaves one member of each group, keeper or not', () => {
    // The suggestion is a suggestion: deleting the keeper and keeping another member is allowed.
    expect(deleteBlock(twoGroups(), new Set(['a1', 'b2', 'b3']))).toBeNull();
  });

  it('refuses more nodes than one delete may name', () => {
    expect(deleteBlock(twoGroups(), new Set(['a2', 'b2', 'b3']), 2)).toEqual({
      key: 'duplicates.block.tooMany',
      max: 2,
    });
  });

  it('uses the server’s cap by default', () => {
    expect(DELETE_MAX).toBe(1000);
  });

  it('has nothing to say about an empty selection', () => {
    expect(deleteBlock(twoGroups(), new Set())).toBeNull();
  });
});

describe('deleteTargets', () => {
  it('names the selected nodes in the order the table lists them, and only listed ones', () => {
    expect(deleteTargets(twoGroups(), new Set(['b3', 'a2', 'gone']))).toEqual([
      { id: 'a2', name: 'node-a2' },
      { id: 'b3', name: 'node-b3' },
    ]);
  });
});

describe('evidenceSummary', () => {
  it('writes each piece of evidence with its label, in the server’s order', () => {
    const g = group([member('a1'), member('a2')], {
      evidence: [
        { kind: 'serial', value: 'FOX1820GVER', node_ids: ['a1', 'a2'] },
        { kind: 'name', value: 'core-sw', node_ids: ['a1', 'a2'] },
      ],
    });
    expect(evidenceSummary(g, (k) => `<${k}>`)).toBe('<serial>: FOX1820GVER · <name>: core-sw');
  });
});

describe('ignoredShown', () => {
  it('counts the rest from the server’s total, not from the capped list', () => {
    const v = view([], {
      ignored: [
        { kind: 'serial', value: 'JPE00000000', nodes: 22 },
        { kind: 'name', value: 'switch', nodes: 9 },
      ],
      ignored_total: 240,
    });
    const { shown, more } = ignoredShown(v, 1);
    expect(shown.map((i) => i.value)).toEqual(['JPE00000000']);
    expect(more).toBe(239);
    expect(ignoredShown(null)).toEqual({ shown: [], more: 0 });
  });
});

describe('emptyState', () => {
  it('says there was nothing to compare with fewer than two nodes', () => {
    expect(emptyState({ scanned: 1, with_serial: 1, with_address_list: 1 })).toEqual({
      key: 'duplicates.emptyNothingToCompare',
      count: 1,
    });
  });

  it('does not read as an all-clear when no serial or address list has been collected', () => {
    expect(emptyState({ scanned: 30, with_serial: 0, with_address_list: 0 })).toEqual({
      key: 'duplicates.emptyNoEvidence',
      count: 30,
    });
  });

  it('says nothing matched once any evidence exists, and before the first read', () => {
    expect(emptyState({ scanned: 30, with_serial: 0, with_address_list: 3 }).key).toBe('duplicates.empty');
    expect(emptyState(null).key).toBe('duplicates.empty');
  });
});
