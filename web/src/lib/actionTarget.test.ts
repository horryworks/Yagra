// SPDX-License-Identifier: AGPL-3.0-only
// The one argument every batch-aware tree action takes (ADR-124 増分 10).
import { describe, expect, it } from 'vitest';
import { targetNodeCount, targetNodeIds, targetNodeNames } from './actionTarget';
import type { NodeSummary } from '../types/api';

const node = (id: string, name = id) => ({ id, name }) as NodeSummary;

describe('targetNodeIds', () => {
  it('lists every node of a set, in the order given', () => {
    // The order is the working set's insertion order, which reaches the server on a move.
    const t = { kind: 'nodes', nodes: [node('c'), node('a'), node('b')] } as const;
    expect(targetNodeIds(t)).toEqual(['c', 'a', 'b']);
  });

  it('is the single id for one row', () => {
    expect(targetNodeIds({ kind: 'node', id: 'n1', name: 'n1' })).toEqual(['n1']);
  });

  it('is empty for a folder', () => {
    // 🚨 Not "every node under the folder". A folder-wide write goes through the folder's own
    // endpoint and reaches its members by inheritance; listing ids here would pin the ones the
    // browser happens to have loaded and silently miss the rest.
    expect(targetNodeIds({ kind: 'group', id: 'g1', name: 'Tokyo' })).toEqual([]);
  });
});

describe('targetNodeCount', () => {
  it('counts the set, the row, and nothing for a folder', () => {
    expect(targetNodeCount({ kind: 'nodes', nodes: [node('a'), node('b')] })).toBe(2);
    expect(targetNodeCount({ kind: 'node', id: 'a', name: 'a' })).toBe(1);
    expect(targetNodeCount({ kind: 'group', id: 'g', name: 'g' })).toBe(0);
  });
});

describe('targetNodeNames', () => {
  it('names a set, and leaves a single target to its own title', () => {
    expect(targetNodeNames({ kind: 'nodes', nodes: [node('a', 'core-1')] })).toEqual(['core-1']);
    expect(targetNodeNames({ kind: 'node', id: 'a', name: 'core-1' })).toEqual([]);
    expect(targetNodeNames({ kind: 'group', id: 'g', name: 'Tokyo' })).toEqual([]);
  });
});
