// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { layoutTopology, NODE_H, NODE_W } from './layout';
import type { TopologyNode } from '../../types/api';

function node(
  id: string,
  parent_id: string | null,
  extra: Partial<TopologyNode> = {},
): TopologyNode {
  return { id, name: id.toUpperCase(), parent_id, state: 'ok', root_cause: null, ...extra };
}

describe('layoutTopology', () => {
  it('places a simple parent → 2 children tree with the parent centered above', () => {
    const { nodes, edges, isolatedCount } = layoutTopology([
      node('p', null),
      node('a', 'p'),
      node('b', 'p'),
    ]);
    expect(isolatedCount).toBe(0);
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const p = byId.get('p')!;
    const a = byId.get('a')!;
    const b = byId.get('b')!;
    // Parent horizontally centered over its two children.
    expect(p.cx).toBeCloseTo((a.cx + b.cx) / 2);
    // Children sit one row below the parent.
    expect(a.cy).toBeGreaterThan(p.cy);
    expect(a.cy).toBe(b.cy);
    // Two edges, parent → each child, connecting box bottom to box top.
    expect(edges).toHaveLength(2);
    for (const e of edges) {
      expect(e.y1).toBeCloseTo(p.cy + NODE_H / 2);
      expect(e.y2).toBeCloseTo(a.cy - NODE_H / 2);
    }
  });

  it('excludes isolated (parentless, childless) nodes from the diagram but counts them', () => {
    const { nodes, edges, isolatedCount, width, height } = layoutTopology([
      node('lonely1', null),
      node('lonely2', null),
    ]);
    expect(nodes).toHaveLength(0);
    expect(edges).toHaveLength(0);
    expect(isolatedCount).toBe(2);
    expect(width).toBe(0);
    expect(height).toBe(0);
  });

  it('lays sibling trees side by side without overlapping columns', () => {
    // Two independent parent→child trees.
    const { nodes } = layoutTopology([
      node('p1', null),
      node('c1', 'p1'),
      node('p2', null),
      node('c2', 'p2'),
    ]);
    const byId = new Map(nodes.map((n) => [n.id, n]));
    // Tree 2 is entirely to the right of tree 1 (at least one column gap).
    const tree1Right = Math.max(byId.get('p1')!.cx, byId.get('c1')!.cx);
    const tree2Left = Math.min(byId.get('p2')!.cx, byId.get('c2')!.cx);
    expect(tree2Left).toBeGreaterThan(tree1Right + NODE_W);
  });

  it('marks the child edge and node suppressed when a root cause is attributed', () => {
    const { nodes, edges } = layoutTopology([
      node('core', null),
      node('leaf', 'core', { root_cause: 'core', state: 'unreachable' }),
    ]);
    const leaf = nodes.find((n) => n.id === 'leaf')!;
    expect(leaf.suppressed).toBe(true);
    expect(leaf.rootCause).toBe('core');
    expect(edges[0].suppressed).toBe(true);
  });

  it('is cycle-safe: a parent loop does not hang or duplicate nodes', () => {
    // a → b → a is a config error; buildForest keeps each node once, layout must terminate.
    const { nodes } = layoutTopology([node('a', 'b'), node('b', 'a')]);
    const ids = nodes.map((n) => n.id).sort();
    expect(new Set(ids).size).toBe(ids.length); // no duplicates
  });

  it('produces a positive canvas size that contains every node box', () => {
    const { nodes, width, height } = layoutTopology([
      node('p', null),
      node('a', 'p'),
      node('b', 'p'),
    ]);
    expect(width).toBeGreaterThan(0);
    expect(height).toBeGreaterThan(0);
    for (const n of nodes) {
      expect(n.cx + NODE_W / 2).toBeLessThanOrEqual(width);
      expect(n.cy + NODE_H / 2).toBeLessThanOrEqual(height);
      expect(n.cx - NODE_W / 2).toBeGreaterThanOrEqual(0);
    }
  });
});
