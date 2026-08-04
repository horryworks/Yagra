// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  classifyNodes,
  verdictCounts,
  canEnableDerived,
  DIFF_VERDICTS,
  type DiffVerdict,
} from './topologyDiff';

const edge = (child: string, parent: string) => ({ child, parent });

describe('classifyNodes', () => {
  it('classifies every node in the inventory, not only the ones that differ', () => {
    // The nodes that *agree* are the majority, and they are the ones a naive implementation drops —
    // leaving a page that looks like the two graphs disagree everywhere.
    const rows = classifyNodes(
      { only_in_manual: [edge('b', 'a')], only_in_derived: [] },
      ['a', 'b', 'c', 'd'],
      new Set(['b', 'c']),
    );
    expect(rows.map((r) => r.nodeId)).toEqual(['a', 'b', 'c', 'd']);
  });

  it('separates a modelled node that agrees from one nothing has an opinion about', () => {
    // The distinction the whole ADR exists for: `unmodelled` is the state the entire fleet was in
    // while `nodes.parent_id` went unfilled, and it must not read as "both graphs agree".
    const rows = classifyNodes({ only_in_manual: [], only_in_derived: [] }, ['a', 'b'], new Set(['a']));
    expect(rows.find((r) => r.nodeId === 'a')?.verdict).toBe('agree');
    expect(rows.find((r) => r.nodeId === 'b')?.verdict).toBe('unmodelled');
  });

  it('reports an upstream only the hand-authored graph has', () => {
    const rows = classifyNodes(
      { only_in_manual: [edge('b', 'a')], only_in_derived: [] },
      ['b'],
      new Set(['b']),
    );
    expect(rows[0]).toEqual({
      nodeId: 'b',
      verdict: 'only_manual',
      manualOnly: ['a'],
      derivedOnly: [],
    });
  });

  it('reports an upstream only the derived graph has', () => {
    const rows = classifyNodes(
      { only_in_manual: [], only_in_derived: [edge('b', 'a')] },
      ['b'],
      new Set(),
    );
    expect(rows[0].verdict).toBe('only_derived');
    expect(rows[0].derivedOnly).toEqual(['a']);
  });

  it('calls a node that differs in both directions only_derived', () => {
    // A row that says "both" says neither. The decision being made is whether to enable the derived
    // graph, and a gained edge can suppress a real outage while a lost one can only make noise — so
    // the risky direction is the one that names the row.
    const rows = classifyNodes(
      { only_in_manual: [edge('c', 'a')], only_in_derived: [edge('c', 'b')] },
      ['c'],
      new Set(['c']),
    );
    expect(rows[0].verdict).toBe('only_derived');
    expect(rows[0].manualOnly).toEqual(['a']);
    expect(rows[0].derivedOnly).toEqual(['b']);
  });

  it('collects every differing parent of a multi-parent node', () => {
    // Multi-parent is the point of the derived graph (a redundant pair), so a classifier that kept
    // only the last parent would hide exactly the case the feature was built for.
    const rows = classifyNodes(
      { only_in_manual: [], only_in_derived: [edge('c', 'a'), edge('c', 'b')] },
      ['c'],
      new Set(),
    );
    expect(rows[0].derivedOnly).toEqual(['a', 'b']);
  });

  it('tolerates a response with the difference lists absent', () => {
    const rows = classifyNodes({} as never, ['a'], new Set());
    expect(rows[0].verdict).toBe('unmodelled');
  });
});

describe('verdictCounts', () => {
  it('includes every verdict even at zero, so a tally never has a missing key', () => {
    const counts = verdictCounts([
      { nodeId: 'a', verdict: 'agree', manualOnly: [], derivedOnly: [] },
      { nodeId: 'b', verdict: 'agree', manualOnly: [], derivedOnly: [] },
    ]);
    expect(Object.keys(counts).sort()).toEqual([...DIFF_VERDICTS].sort());
    expect(counts.agree).toBe(2);
    expect(counts.only_derived).toBe(0);
  });

  it('counts each row exactly once across the verdicts', () => {
    const rows = DIFF_VERDICTS.map((v: DiffVerdict, i) => ({
      nodeId: String(i),
      verdict: v,
      manualOnly: [],
      derivedOnly: [],
    }));
    const counts = verdictCounts(rows);
    expect(Object.values(counts).reduce((a, b) => a + b, 0)).toBe(rows.length);
  });
});

describe('canEnableDerived', () => {
  it('blocks while a pool has an unplaced poller, and allows once none do', () => {
    // Mirrors the server's 409 rather than adding a second rule — an enabled button whose call
    // always fails is worse than a disabled one with the reason beside it.
    expect(canEnableDerived({ unresolved_pools: ['default'] })).toBe(false);
    expect(canEnableDerived({ unresolved_pools: [] })).toBe(true);
    expect(canEnableDerived({} as never)).toBe(true);
  });
});
