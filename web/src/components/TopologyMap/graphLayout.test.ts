// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { layoutGraph, MAX_GRAPH_NODES, NODE_H, NODE_W } from './graphLayout';
import type { TopologyLink, TopologyNode } from '../../types/api';

function node(id: string, extra: Partial<TopologyNode> = {}): TopologyNode {
  return {
    id,
    name: `node-${id}`,
    parent_id: null,
    state: 'ok',
    root_cause: null,
    ...extra,
  } as TopologyNode;
}

let nextLinkId = 1;
function link(a: string, b: string, extra: Partial<TopologyLink> = {}): TopologyLink {
  return {
    id: nextLinkId++,
    a_node: a,
    b_node: b,
    a_ifindex: null,
    b_ifindex: null,
    a_if_name: null,
    b_if_name: null,
    sources: ['l3_subnet'],
    source: 'l3_subnet',
    subnet: '10.0.0.0/24',
    first_seen: '2026-08-04T00:00:00Z',
    last_seen: '2026-08-04T01:00:00Z',
    ...extra,
  } as TopologyLink;
}

/** A router with three servers hanging off it, plus a second router — the shape the derivation
 *  produces for an ordinary segment. */
function segment() {
  const nodes = ['r1', 'r2', 's1', 's2', 's3'].map((id) => node(id));
  const links = [
    link('r1', 'r2'),
    link('r1', 's1'),
    link('r2', 's1'),
    link('r1', 's2'),
    link('r2', 's2'),
    link('r1', 's3'),
    link('r2', 's3'),
  ];
  return { nodes, links };
}

function shuffle<T>(items: T[], seed: number): T[] {
  // A fixed permutation, not a random one — a test that shuffles randomly fails intermittently and
  // gets deleted rather than debugged.
  const out = [...items];
  for (let i = out.length - 1; i > 0; i--) {
    const j = (i * seed + 7) % (i + 1);
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

describe('layoutGraph', () => {
  it('is invariant under input order', () => {
    // The map re-fetches on a timer and the server's row order is not a promise. If the layout
    // followed it, the whole diagram would rearrange itself every cycle.
    const { nodes, links } = segment();
    const a = layoutGraph({ nodes, links });
    const b = layoutGraph({ nodes: shuffle(nodes, 3), links: shuffle(links, 5) });
    expect(b).toEqual(a);
  });

  it('is idempotent', () => {
    const { nodes, links } = segment();
    expect(layoutGraph({ nodes, links })).toEqual(layoutGraph({ nodes, links }));
  });

  it('produces unchanged coordinates for an unchanged graph', () => {
    // The property the 15-second poll depends on: same graph in, same pixels out.
    const { nodes, links } = segment();
    const first = layoutGraph({ nodes, links });
    const second = layoutGraph({ nodes: [...nodes], links: [...links] });
    for (const n of first.nodes) {
      const same = second.nodes.find((m) => m.id === n.id)!;
      expect([same.cx, same.cy]).toEqual([n.cx, n.cy]);
    }
  });

  it('keeps redundant links instead of collapsing them to a tree', () => {
    // The reason the tidy-tree had to go: each server reaches both routers, and both edges must
    // survive — that is what makes multi-parent suppression meaningful downstream.
    const { nodes, links } = segment();
    const out = layoutGraph({ nodes, links });
    expect(out.edges).toHaveLength(links.length);
    for (const s of ['s1', 's2', 's3']) {
      const touching = out.edges.filter((e) => e.id.includes(s));
      expect(touching).toHaveLength(2);
    }
  });

  it('draws a same-rank edge as a bow rather than straight through the boxes between', () => {
    const { nodes, links } = segment();
    const out = layoutGraph({ nodes, links });
    const bows = out.edges.filter((e) => e.kind === 'bow');
    expect(bows.length).toBeGreaterThan(0);
    for (const b of bows) expect(b.path).toMatch(/^M .* Q .*/);
    // A straight edge must not carry a path, or the renderer would draw it twice.
    for (const l of out.edges.filter((e) => e.kind === 'line')) expect(l.path).toBeUndefined();
  });

  it('terminates on a cycle', () => {
    const nodes = ['a', 'b', 'c'].map((id) => node(id));
    const links = [link('a', 'b'), link('b', 'c'), link('c', 'a')];
    const out = layoutGraph({ nodes, links });
    expect(out.nodes).toHaveLength(3);
    expect(out.edges).toHaveLength(3);
  });

  it('lays disconnected components out without overlapping them', () => {
    // Multiple components are normal, not an error: a device that speaks no discovery protocol is
    // its own island, and so is a site reached over a link nothing reported.
    const nodes = ['a', 'b', 'x', 'y'].map((id) => node(id));
    const links = [link('a', 'b'), link('x', 'y')];
    const out = layoutGraph({ nodes, links });
    expect(out.componentCount).toBe(2);
    const seen = new Set(out.nodes.map((n) => `${n.cx},${n.cy}`));
    expect(seen.size).toBe(out.nodes.length);
    // And no two boxes share a column at the same row.
    for (const a of out.nodes) {
      for (const b of out.nodes) {
        if (a.id === b.id) continue;
        const overlaps = Math.abs(a.cx - b.cx) < NODE_W && Math.abs(a.cy - b.cy) < NODE_H;
        expect(overlaps).toBe(false);
      }
    }
  });

  it('counts nodes with no link rather than drawing a field of loose boxes', () => {
    const nodes = ['a', 'b', 'lonely'].map((id) => node(id));
    const out = layoutGraph({ nodes, links: [link('a', 'b')] });
    expect(out.isolatedCount).toBe(1);
    expect(out.nodes.map((n) => n.id)).toEqual(['a', 'b']);
  });

  it('handles an empty graph', () => {
    const out = layoutGraph({ nodes: [], links: [] });
    expect(out.nodes).toHaveLength(0);
    expect(out.edges).toHaveLength(0);
    expect(out.width).toBe(0);
    expect(out.height).toBe(0);
    expect(out.componentCount).toBe(0);
  });

  it('ignores a link whose endpoint is not a node in the graph', () => {
    // Increment 3 introduces endpoints that are not monitored nodes, and a scoped caller can
    // already receive a page where one end was filtered out. Neither may throw.
    const nodes = [node('a'), node('b')];
    const links = [
      link('a', 'b'),
      link('a', 'ghost'),
      { ...link('a', 'b'), a_node: null } as TopologyLink,
      { ...link('a', 'b'), b_node: null } as TopologyLink,
    ];
    const out = layoutGraph({ nodes, links });
    expect(out.edges).toHaveLength(1);
  });

  it('ignores a self-link', () => {
    const out = layoutGraph({ nodes: [node('a')], links: [link('a', 'a')] });
    expect(out.edges).toHaveLength(0);
    expect(out.isolatedCount).toBe(1);
  });

  it('collapses a duplicated link to one edge', () => {
    // The server dedups, but the two directions of the same pair must not produce two edges here
    // either — a doubled edge would render twice and count twice.
    const nodes = [node('a'), node('b')];
    const out = layoutGraph({ nodes, links: [link('a', 'b'), link('b', 'a')] });
    expect(out.edges).toHaveLength(1);
  });

  it('roots the layout at the supplied anchor when there is one', () => {
    // Increment 2 passes the poller-derived anchor; until then the highest-degree node stands in.
    // This pins that the parameter is honoured, so that change stays a caller change.
    const { nodes, links } = segment();
    const anchored = layoutGraph({ nodes, links, anchorId: 's1' });
    const top = anchored.nodes.reduce((a, b) => (a.cy <= b.cy ? a : b));
    expect(top.id).toBe('s1');
  });

  it('carries the evidence through to the edge so the map can label it', () => {
    const nodes = [node('a'), node('b')];
    const out = layoutGraph({
      nodes,
      links: [link('a', 'b', { source: 'lldp', sources: ['lldp', 'l3_subnet'] })],
    });
    expect(out.edges[0].source).toBe('lldp');
  });

  it('marks an edge suppressed when its downstream end is under a root cause', () => {
    const nodes = [node('a'), node('b', { root_cause: 'a' })];
    const out = layoutGraph({ nodes, links: [link('a', 'b')], anchorId: 'a' });
    expect(out.edges[0].suppressed).toBe(true);
    expect(out.nodes.find((n) => n.id === 'b')!.suppressed).toBe(true);
  });

  it('lays out a fleet-sized graph without exploding', () => {
    // A star of MAX_GRAPH_NODES-1 leaves: the worst realistic shape for the barycentre pass, since
    // every leaf shares one rank.
    const nodes = [node('hub')];
    const links: TopologyLink[] = [];
    for (let i = 0; i < MAX_GRAPH_NODES - 1; i++) {
      nodes.push(node(`n${String(i).padStart(5, '0')}`));
      links.push(link('hub', `n${String(i).padStart(5, '0')}`));
    }
    const out = layoutGraph({ nodes, links });
    expect(out.nodes).toHaveLength(MAX_GRAPH_NODES);
    expect(out.edges).toHaveLength(MAX_GRAPH_NODES - 1);
    expect(out.componentCount).toBe(1);
  });
});
