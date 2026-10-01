// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  BOX_SIZE,
  CELL_H,
  CELL_W,
  NODE_TALL,
  layoutGraph,
  type GraphLink,
  type GraphNode,
} from './graphLayout';

function node(id: string, extra: Partial<GraphNode> = {}): GraphNode {
  return {
    id,
    kind: 'node',
    name: `node-${id}`,
    state: 'ok',
    sub: null,
    rootCause: null,
    ...extra,
  };
}

let nextLinkId = 1;
function link(a: string, b: string, extra: Partial<GraphLink> = {}): GraphLink {
  return {
    id: `l${String(nextLinkId++).padStart(6, '0')}`,
    a,
    b,
    source: 'l3_subnet',
    count: 1,
    ...extra,
  };
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
    const drawn = new Set(out.edges.map((e) => e.id));
    for (const s of ['s1', 's2', 's3']) {
      const touching = links.filter((l) => (l.a === s || l.b === s) && drawn.has(l.id));
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
        const overlaps =
          Math.abs(a.cx - b.cx) < BOX_SIZE.node.w && Math.abs(a.cy - b.cy) < BOX_SIZE.node.h;
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

  it('ignores a link whose endpoint is not a box in the graph', () => {
    // A level drawn from a response that was cut short must not throw on a dangling end.
    const nodes = [node('a'), node('b')];
    const links = [link('a', 'b'), link('a', 'ghost'), link('', 'b'), link('a', '')];
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
      links: [link('a', 'b', { source: 'lldp', count: 3 })],
    });
    expect(out.edges[0].source).toBe('lldp');
    expect(out.edges[0].count).toBe(3);
  });

  it('marks an edge suppressed when its downstream end is under a root cause', () => {
    const nodes = [node('a'), node('b', { rootCause: 'a' })];
    const out = layoutGraph({ nodes, links: [link('a', 'b')], anchorId: 'a' });
    expect(out.edges[0].suppressed).toBe(true);
    expect(out.nodes.find((n) => n.id === 'b')!.suppressed).toBe(true);
  });

  it('lays out a fleet-sized graph without exploding', () => {
    // A star of 1999 leaves (the server draws at most 2000 linked nodes per level): the worst
    // realistic shape for the barycentre pass, since every leaf shares one rank.
    const MAX = 2000;
    const nodes = [node('hub')];
    const links: GraphLink[] = [];
    for (let i = 0; i < MAX - 1; i++) {
      nodes.push(node(`n${String(i).padStart(5, '0')}`));
      links.push(link('hub', `n${String(i).padStart(5, '0')}`));
    }
    const out = layoutGraph({ nodes, links });
    expect(out.nodes).toHaveLength(MAX);
    expect(out.edges).toHaveLength(MAX - 1);
    expect(out.componentCount).toBe(1);
  });

  it('places a folder with no link, and counts only nodes as isolated', () => {
    // A subfolder's box is the way down a level, so it is drawn even when nothing links to it.
    const nodes = [
      node('a'),
      node('b'),
      node('lonely'),
      node('f1', { kind: 'folder', name: 'site-b' }),
      node('f0', { kind: 'folder', name: 'site-a' }),
    ];
    const out = layoutGraph({ nodes, links: [link('a', 'b')] });
    expect(out.isolatedCount).toBe(1);
    const ids = out.nodes.map((n) => n.id);
    expect(ids).toContain('f0');
    expect(ids).toContain('f1');
    expect(ids).not.toContain('lonely');
    // Below the linked component, in name order.
    const f0 = out.nodes.find((n) => n.id === 'f0')!;
    const f1 = out.nodes.find((n) => n.id === 'f1')!;
    const a = out.nodes.find((n) => n.id === 'a')!;
    expect(f0.cy).toBeGreaterThan(a.cy);
    expect(f0.cx).toBeLessThan(f1.cx);
  });

  it('lays out a level of only unlinked folders deterministically', () => {
    const folders = ['c', 'a', 'b', 'e', 'd'].map((id) => node(id, { kind: 'folder', name: id }));
    const one = layoutGraph({ nodes: folders, links: [] });
    const two = layoutGraph({ nodes: shuffle(folders, 3), links: [] });
    expect(two).toEqual(one);
    expect(one.nodes).toHaveLength(5);
    expect(one.width).toBeGreaterThan(0);
  });

  it('sizes each box by its kind and ends a line on each box edge', () => {
    const nodes = [node('n'), node('f', { kind: 'folder' })];
    const out = layoutGraph({ nodes, links: [link('n', 'f')], anchorId: 'n' });
    const n = out.nodes.find((x) => x.id === 'n')!;
    const f = out.nodes.find((x) => x.id === 'f')!;
    expect([n.w, n.h]).toEqual([BOX_SIZE.node.w, BOX_SIZE.node.h]);
    expect([f.w, f.h]).toEqual([BOX_SIZE.folder.w, BOX_SIZE.folder.h]);
    const e = out.edges[0];
    expect(e.kind).toBe('line');
    expect(e.y1).toBe(n.cy + BOX_SIZE.node.h / 2);
    expect(e.y2).toBe(f.cy - BOX_SIZE.folder.h / 2);
    expect(e.chip).toEqual({ x: (e.x1 + e.x2) / 2, y: (e.y1 + e.y2) / 2 });
  });

  it('puts the chip of a bowed edge on the curve', () => {
    const { nodes, links } = segment();
    const out = layoutGraph({ nodes, links });
    const bow = out.edges.find((e) => e.kind === 'bow')!;
    const [, mx, my] = /Q (\S+) (\S+)/.exec(bow.path!)!.map(Number);
    expect(bow.chip.x).toBeCloseTo(0.25 * bow.x1 + 0.5 * mx + 0.25 * bow.x2);
    expect(bow.chip.y).toBeCloseTo(0.25 * bow.y1 + 0.5 * my + 0.25 * bow.y2);
  });

  it('makes every grid cell fit the largest box', () => {
    for (const k of ['node', 'folder', 'external'] as const) {
      expect(CELL_W).toBeGreaterThan(BOX_SIZE[k].w);
      expect(CELL_H).toBeGreaterThan(BOX_SIZE[k].h);
    }
    expect(CELL_H).toBeGreaterThan(NODE_TALL.h);
  });

  it('draws a node with a second line taller, and ends its lines on its own edge', () => {
    // On a level drawn flat, a node carries the subfolder it is filed in (ADR-191 Inc.2).
    const nodes = [node('a'), node('b', { sub: 'floor-1' })];
    const out = layoutGraph({ nodes, links: [link('a', 'b')], anchorId: 'a' });
    const b = out.nodes.find((n) => n.id === 'b')!;
    expect([b.w, b.h]).toEqual([NODE_TALL.w, NODE_TALL.h]);
    expect(out.edges[0].y2).toBe(b.cy - NODE_TALL.h / 2);
    expect(out.nodes.find((n) => n.id === 'a')!.h).toBe(BOX_SIZE.node.h);
  });
});
