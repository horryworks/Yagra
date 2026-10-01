// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  AP_LINE_H,
  AP_PER_LINE,
  AP_PITCH,
  AP_SIZE,
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
    ap: false,
    role: 'other',
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

describe('layoutGraph — access points (ADR-191 Inc.5)', () => {
  const ap = (id: string, extra: Partial<GraphNode> = {}) =>
    node(id, { ap: true, role: 'access_point', ...extra });
  const byId = (out: ReturnType<typeof layoutGraph>, id: string) => out.nodes.find((n) => n.id === id)!;

  /** A core switch, two access switches, and the given number of APs on each. */
  function floor(perSwitch: number) {
    const nodes = [node('core'), node('sw1'), node('sw2')];
    const links = [link('core', 'sw1'), link('core', 'sw2')];
    for (const sw of ['sw1', 'sw2']) {
      for (let i = 0; i < perSwitch; i++) {
        const id = `${sw}-ap${String(i).padStart(2, '0')}`;
        nodes.push(ap(id, { name: id }));
        links.push(link(sw, id));
      }
    }
    return { nodes, links };
  }

  it('keeps the old grid exactly when a level has no access point', () => {
    const out = layoutGraph({ nodes: [node('a'), node('b')], links: [link('a', 'b')], anchorId: 'a' });
    expect([byId(out, 'a').cx, byId(out, 'a').cy]).toEqual([24 + 100, 24 + 38]);
    expect([byId(out, 'b').cx, byId(out, 'b').cy]).toEqual([24 + 100, 24 + CELL_H + 38]);
  });

  it('hangs each access point in the band below its parent, centred under it', () => {
    const out = layoutGraph(floor(3));
    for (const sw of ['sw1', 'sw2']) {
      const parent = byId(out, sw);
      const aps = out.nodes.filter((n) => n.id.startsWith(`${sw}-ap`));
      expect(aps).toHaveLength(3);
      for (const a of aps) {
        expect(a.cy).toBeGreaterThan(parent.cy + parent.h / 2);
        expect([a.w, a.h]).toEqual([AP_SIZE.w, AP_SIZE.h]);
      }
      const mean = aps.reduce((s, a) => s + a.cx, 0) / aps.length;
      expect(mean).toBeCloseTo(parent.cx);
      const xs = aps.map((a) => a.cx).sort((x, y) => x - y);
      expect(xs[1] - xs[0]).toBe(AP_PITCH);
    }
  });

  it('draws the line to the parent straight, from the box to the top of the circle', () => {
    const out = layoutGraph(floor(1));
    const parent = byId(out, 'sw1');
    const a = byId(out, 'sw1-ap00');
    const e = out.edges.find((x) => [x.x1, x.x2].includes(a.cx) && x.y2 === a.cy - AP_SIZE.h / 2)!;
    expect(e.kind).toBe('line');
    expect([e.x1, e.y1]).toEqual([parent.cx, parent.cy + parent.h / 2]);
  });

  it('never roots the layout at an access point, however many links it has', () => {
    const nodes = [node('sw'), ap('a1'), ap('a2'), ap('a3'), ap('a4')];
    const links = [link('sw', 'a1'), link('a1', 'a2'), link('a1', 'a3'), link('a1', 'a4')];
    const out = layoutGraph({ nodes, links });
    const top = Math.min(...out.nodes.map((n) => n.cy));
    expect(byId(out, 'sw').cy).toBe(top);
    // The mesh repeaters hang beside the AP they repeat, under its switch.
    for (const id of ['a2', 'a3', 'a4']) expect(byId(out, id).cy).toBe(byId(out, 'a1').cy);
  });

  it('puts an access point linked to two switches under the higher one', () => {
    const nodes = [node('core'), node('dist'), node('acc'), ap('a')];
    const links = [link('core', 'dist'), link('dist', 'acc'), link('dist', 'a'), link('acc', 'a')];
    const out = layoutGraph({ nodes, links, anchorId: 'core' });
    const a = byId(out, 'a');
    expect(a.cx).toBe(byId(out, 'dist').cx);
    expect(a.cy).toBeGreaterThan(byId(out, 'dist').cy);
    expect(a.cy).toBeLessThan(byId(out, 'acc').cy);
    // The second link is a bow, not a line through the boxes between.
    const second = out.edges.find((e) => e.kind === 'bow')!;
    expect(second.path).toMatch(/^M .* Q .*/);
  });

  it('draws a switch whose only links go to access points', () => {
    const out = layoutGraph({ nodes: [node('sw'), ap('a')], links: [link('sw', 'a')] });
    expect(out.nodes.map((n) => n.id).sort()).toEqual(['a', 'sw']);
    expect(out.isolatedCount).toBe(0);
  });

  it('still counts an access point with no link instead of drawing it', () => {
    const out = layoutGraph({ nodes: [node('sw'), node('r'), ap('lonely')], links: [link('sw', 'r')] });
    expect(out.nodes.map((n) => n.id).sort()).toEqual(['r', 'sw']);
    expect(out.isolatedCount).toBe(1);
  });

  it('lays out an island of access points with no switch like any box', () => {
    const out = layoutGraph({ nodes: [ap('a'), ap('b')], links: [link('a', 'b')] });
    expect(out.nodes).toHaveLength(2);
    expect(out.edges).toHaveLength(1);
    expect(out.componentCount).toBe(1);
  });

  it(`wraps a group after ${AP_PER_LINE} and keeps neighbouring groups apart`, () => {
    const out = layoutGraph(floor(AP_PER_LINE + 1));
    const g1 = out.nodes.filter((n) => n.id.startsWith('sw1-ap'));
    const lines = [...new Set(g1.map((n) => n.cy))].sort((x, y) => x - y);
    expect(lines).toHaveLength(2);
    expect(g1.filter((n) => n.cy === lines[0])).toHaveLength(AP_PER_LINE);
    expect(lines[1] - lines[0]).toBe(AP_LINE_H);
    // No two drawn things overlap, labels' width included for the circles.
    const rects = out.nodes.map((n) => {
      const w = n.ap ? AP_PITCH : n.w;
      return { id: n.id, l: n.cx - w / 2, r: n.cx + w / 2, t: n.cy - n.h / 2, b: n.cy + n.h / 2 };
    });
    for (let i = 0; i < rects.length; i++) {
      for (let j = i + 1; j < rects.length; j++) {
        const [p, q] = [rects[i], rects[j]];
        const overlap = p.l < q.r && q.l < p.r && p.t < q.b && q.t < p.b;
        expect(overlap, `${p.id} overlaps ${q.id}`).toBe(false);
      }
    }
    for (const n of out.nodes) {
      expect(n.cx + (n.ap ? AP_PITCH : n.w) / 2).toBeLessThanOrEqual(out.width);
      expect(n.cy + n.h / 2).toBeLessThanOrEqual(out.height);
    }
  });

  it('orders a group by name and does not depend on input order', () => {
    const { nodes, links } = floor(5);
    const one = layoutGraph({ nodes, links });
    const two = layoutGraph({ nodes: shuffle(nodes, 5), links: shuffle(links, 11) });
    expect(two).toEqual(one);
    const g = one.nodes.filter((n) => n.id.startsWith('sw1-ap')).sort((x, y) => x.cx - y.cx);
    expect(g.map((n) => n.name)).toEqual([...g.map((n) => n.name)].sort());
  });
});

describe('layoutGraph — role rows (ADR-191 Inc.6)', () => {
  const as = (role: GraphNode['role']) => (id: string) => node(id, { role });
  const edge = as('edge');
  const l3 = as('l3_switch');
  const l2 = as('l2_switch');

  /** Each backbone box's row, numbered from 0 at the top. Rows are read off the centres, which
   *  only ever grow with the row. */
  function rows(out: ReturnType<typeof layoutGraph>): Record<string, number> {
    const boxes = out.nodes.filter((n) => !n.ap);
    const ys = [...new Set(boxes.map((n) => n.cy))].sort((a, b) => a - b);
    return Object.fromEntries(boxes.map((n) => [n.id, ys.indexOf(n.cy)]));
  }

  it('puts the router on top even when the core switch has the most links', () => {
    const nodes = [edge('rt'), l3('core'), l2('a1'), l2('a2'), l2('a3')];
    const links = [link('rt', 'core'), link('core', 'a1'), link('core', 'a2'), link('core', 'a3')];
    expect(rows(layoutGraph({ nodes, links }))).toEqual({ rt: 0, core: 1, a1: 2, a2: 2, a3: 2 });
    // The same graph with no roles roots at the best-connected box, as before.
    const plain = nodes.map((n) => ({ ...n, role: 'other' as const }));
    expect(rows(layoutGraph({ nodes: plain, links })).core).toBe(0);
  });

  it('lays out a graph from an N-1 core, which sends no role, as if every role were other', () => {
    const nodes = [edge('rt'), l3('core'), l2('a1'), l2('a2')];
    const links = [link('rt', 'core'), link('core', 'a1'), link('core', 'a2')];
    const plain = nodes.map((n) => ({ ...n, role: 'other' as const }));
    const missing = nodes.map((n) => ({ ...n, role: undefined }) as unknown as GraphNode);
    const at = (out: ReturnType<typeof layoutGraph>) => out.nodes.map((n) => [n.id, n.cx, n.cy]);
    expect(at(layoutGraph({ nodes: missing, links }))).toEqual(at(layoutGraph({ nodes: plain, links })));
  });

  /** One real site's shape: a router and two firewalls, a core that routes, five access switches,
   *  a wireless controller and thirteen APs — plus the five lines CDP reports from the router to
   *  each access switch's uplink, through a core that forwards CDP without speaking it. */
  function site() {
    const access = ['as1', 'as2', 'as3', 'as4', 'as5'];
    const nodes = [edge('rt'), edge('fw1'), edge('fw2'), l3('core'), ...access.map(l2), node('wlc')];
    const links = [
      link('rt', 'core'),
      link('fw1', 'core'),
      link('fw2', 'core'),
      link('core', 'wlc'),
      link('rt', 'wlc', { source: 'cdp' }),
      ...access.map((a) => link('core', a)),
      ...access.map((a) => link('rt', a, { id: `cdp-rt-${a}`, source: 'cdp' })),
    ];
    for (let i = 0; i < 13; i++) {
      const id = `ap${String(i).padStart(2, '0')}`;
      nodes.push(node(id, { ap: true, role: 'access_point', name: id }));
      links.push(link(access[i % 4], id, { source: 'cdp' }));
    }
    return { nodes, links };
  }

  it('draws a site as router and firewalls, core, access switches, then APs', () => {
    const out = layoutGraph(site());
    expect(rows(out)).toEqual({
      rt: 0,
      fw1: 0,
      fw2: 0,
      core: 1,
      // The controller sits one row under the nearest box it is linked to — the router here.
      wlc: 1,
      as1: 2,
      as2: 2,
      as3: 2,
      as4: 2,
      as5: 2,
    });
    const at = (id: string) => out.nodes.find((n) => n.id === id)!;
    for (const n of out.nodes.filter((x) => x.ap)) {
      const parent = at(`as${(Number(n.id.slice(2)) % 4) + 1}`);
      expect(n.cy).toBeGreaterThan(parent.cy);
    }
    // A line that skips the core row bows rather than running through it.
    const skip = out.edges.filter((e) => e.id.startsWith('cdp-rt-'));
    expect(skip).toHaveLength(5);
    expect(skip.every((e) => e.kind === 'bow')).toBe(true);
  });

  it('leaves no empty row when there is no router', () => {
    const nodes = [l3('core'), l2('a1'), l2('a2')];
    const links = [link('core', 'a1'), link('core', 'a2')];
    expect(rows(layoutGraph({ nodes, links }))).toEqual({ core: 0, a1: 1, a2: 1 });
  });

  it('steps a daisy-chained access switch down under the one it hangs off', () => {
    const nodes = [edge('rt'), l3('core'), l2('as1'), l2('as2')];
    const links = [link('rt', 'core'), link('core', 'as1'), link('as1', 'as2')];
    expect(rows(layoutGraph({ nodes, links }))).toEqual({ rt: 0, core: 1, as1: 2, as2: 3 });
  });

  it('puts a folder box and a chain of other devices under what they are linked to', () => {
    const nodes = [
      edge('rt'),
      node('srv1'),
      node('srv2'),
      { ...node('f'), kind: 'folder' as const, name: 'floor-1' },
    ];
    const links = [link('rt', 'srv1'), link('srv1', 'srv2'), link('rt', 'f')];
    expect(rows(layoutGraph({ nodes, links }))).toEqual({ rt: 0, srv1: 1, f: 1, srv2: 2 });
  });

  it('does not depend on input order', () => {
    const { nodes, links } = site();
    const one = layoutGraph({ nodes, links });
    const two = layoutGraph({ nodes: shuffle(nodes, 3), links: shuffle(links, 7) });
    expect(two).toEqual(one);
  });

  it('still hangs access points centred under switches that have roles', () => {
    const nodes = [l3('core'), l2('sw1'), l2('sw2')];
    const links = [link('core', 'sw1'), link('core', 'sw2')];
    for (const sw of ['sw1', 'sw2']) {
      for (let i = 0; i < 3; i++) {
        const id = `${sw}-ap${i}`;
        nodes.push(node(id, { ap: true, role: 'access_point', name: id }));
        links.push(link(sw, id));
      }
    }
    const out = layoutGraph({ nodes, links });
    for (const sw of ['sw1', 'sw2']) {
      const parent = out.nodes.find((n) => n.id === sw)!;
      const aps = out.nodes.filter((n) => n.id.startsWith(`${sw}-ap`));
      const mid = aps.reduce((acc, n) => acc + n.cx, 0) / aps.length;
      expect(mid).toBeCloseTo(parent.cx, 6);
      expect(aps.every((n) => n.cy > parent.cy)).toBe(true);
    }
  });
});
