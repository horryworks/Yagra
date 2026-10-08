// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  AP_PITCH,
  AP_SIZE,
  BOX_SIZE,
  BUNDLE_SIZE,
  SIDE_GAP,
  bundleId,
  worstState,
  CELL_H,
  CELL_W,
  NODE_TALL,
  ROW_ORDER,
  ROW_WRAP_MIN,
  SHELF_MIN_COLS,
  layoutGraph,
  rowCap,
  shelfCols,
  wrapRow,
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

describe('layoutGraph — access points (ADR-191 Inc.5, Inc.9)', () => {
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

  /** No two drawn things overlap (a circle counts its label's width), and all fit the canvas. */
  function expectNoOverlap(out: ReturnType<typeof layoutGraph>) {
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
  }

  it('keeps the old grid exactly when a level has no access point', () => {
    const out = layoutGraph({ nodes: [node('a'), node('b')], links: [link('a', 'b')], anchorId: 'a' });
    expect([byId(out, 'a').cx, byId(out, 'a').cy]).toEqual([24 + 100, 24 + 38]);
    expect([byId(out, 'b').cx, byId(out, 'b').cy]).toEqual([24 + 100, 24 + CELL_H + 38]);
  });

  it('draws two or more access points under one parent as one bundle, centred under it', () => {
    const out = layoutGraph(floor(3));
    for (const sw of ['sw1', 'sw2']) {
      const parent = byId(out, sw);
      expect(out.nodes.some((n) => n.id.startsWith(`${sw}-ap`))).toBe(false);
      const b = byId(out, bundleId(sw));
      expect([b.w, b.h]).toEqual([BUNDLE_SIZE.w, BUNDLE_SIZE.h]);
      expect(b.ap).toBe(true);
      expect(b.cx).toBe(parent.cx);
      expect(b.cy).toBeGreaterThan(parent.cy + parent.h / 2);
      expect(b.bundle?.parent).toBe(sw);
      expect(b.bundle?.members.map((m) => m.id)).toEqual([`${sw}-ap00`, `${sw}-ap01`, `${sw}-ap02`]);
    }
  });

  it('colours a bundle with the worst state among its members', () => {
    const { nodes, links } = floor(3);
    const sick = nodes.map((n) =>
      n.id === 'sw1-ap01' ? { ...n, state: 'critical' as const } : n.id === 'sw1-ap02' ? { ...n, state: 'warning' as const } : n,
    );
    const out = layoutGraph({ nodes: sick, links });
    expect(byId(out, bundleId('sw1')).state).toBe('critical');
    expect(byId(out, bundleId('sw2')).state).toBe('ok');
    expect(worstState([])).toBe('ok');
  });

  it('keeps a lone access point as itself, with a straight line from the box to the top of it', () => {
    const out = layoutGraph(floor(1));
    const parent = byId(out, 'sw1');
    const a = byId(out, 'sw1-ap00');
    expect([a.w, a.h]).toEqual([AP_SIZE.w, AP_SIZE.h]);
    expect(a.bundle).toBeUndefined();
    const e = out.edges.find((x) => x.x2 === a.cx && x.y2 === a.cy - AP_SIZE.h / 2)!;
    expect(e.kind).toBe('line');
    expect(e.box).toBeUndefined();
    expect([e.x1, e.y1]).toEqual([parent.cx, parent.cy + parent.h / 2]);
  });

  it('collapses the lines from a parent to its bundle into one, counting every link', () => {
    const out = layoutGraph(floor(3));
    const parent = byId(out, 'sw1');
    const b = byId(out, bundleId('sw1'));
    const lines = out.edges.filter((e) => e.box === b.id);
    expect(lines).toHaveLength(1);
    expect(lines[0].count).toBe(3);
    expect(lines[0].kind).toBe('line');
    expect([lines[0].x1, lines[0].y1, lines[0].x2, lines[0].y2]).toEqual([
      parent.cx,
      parent.cy + parent.h / 2,
      b.cx,
      b.cy - b.h / 2,
    ]);
    // Two backbone lines and one per bundle.
    expect(out.edges).toHaveLength(4);
  });

  it('never roots the layout at an access point, and folds a mesh into its parent bundle', () => {
    const nodes = [node('sw'), ap('a1'), ap('a2'), ap('a3'), ap('a4')];
    const links = [link('sw', 'a1'), link('a1', 'a2'), link('a1', 'a3'), link('a1', 'a4')];
    const out = layoutGraph({ nodes, links });
    expect(out.nodes.map((n) => n.id).sort()).toEqual([bundleId('sw'), 'sw']);
    expect(byId(out, bundleId('sw')).bundle?.members).toHaveLength(4);
    expect(byId(out, 'sw').cy).toBeLessThan(byId(out, bundleId('sw')).cy);
    // The lines between the repeaters are inside the bundle and are not drawn.
    expect(out.edges).toHaveLength(1);
    expect(out.edges[0].count).toBe(1);
  });

  it('puts a lone access point beside a parent that has a child below, and bows its second link', () => {
    const nodes = [node('core'), node('dist'), node('acc'), ap('a')];
    const links = [link('core', 'dist'), link('dist', 'acc'), link('dist', 'a'), link('acc', 'a')];
    const out = layoutGraph({ nodes, links, anchorId: 'core' });
    const dist = byId(out, 'dist');
    const a = byId(out, 'a');
    expect(a.cy).toBe(dist.cy);
    expect(a.cx).toBe(dist.cx + dist.w / 2 + SIDE_GAP + AP_PITCH / 2);
    // The line to the parent runs sideways, from the box's edge to the circle's.
    const side = out.edges.find((e) => e.y1 === dist.cy && e.y2 === dist.cy)!;
    expect([side.x1, side.x2]).toEqual([dist.cx + dist.w / 2, a.cx - a.w / 2]);
    // The second link is a bow, not a line through the boxes between.
    const second = out.edges.find((e) => e.kind === 'bow')!;
    expect(second.path).toMatch(/^M .* Q .*/);
  });

  it('collapses a bundle’s links to a second switch into one bow that selects the bundle', () => {
    const nodes = [node('core'), node('dist'), node('acc'), ap('a'), ap('b')];
    const links = [
      link('core', 'dist'),
      link('dist', 'acc'),
      link('dist', 'a'),
      link('dist', 'b'),
      link('acc', 'a'),
      link('acc', 'b'),
    ];
    const out = layoutGraph({ nodes, links, anchorId: 'core' });
    const b = byId(out, bundleId('dist'));
    expect(b.cy).toBe(byId(out, 'dist').cy);
    const toAcc = out.edges.filter((e) => e.box === b.id && e.kind === 'bow');
    expect(toAcc).toHaveLength(1);
    expect(toAcc[0].count).toBe(2);
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

  it('widens a column for a bundle drawn beside its box, so nothing overlaps', () => {
    const { nodes, links } = floor(2);
    for (let i = 0; i < 5; i++) {
      nodes.push(ap(`core-ap${i}`, { name: `core-ap${i}` }));
      links.push(link('core', `core-ap${i}`));
    }
    const out = layoutGraph({ nodes, links });
    const core = byId(out, 'core');
    const b = byId(out, bundleId('core'));
    expect(b.cy).toBe(core.cy);
    expect(b.cx).toBeGreaterThan(core.cx + core.w / 2);
    expectNoOverlap(out);
    expectNoOverlap(layoutGraph(floor(9)));
  });

  it('orders a bundle by name and does not depend on input order', () => {
    const { nodes, links } = floor(5);
    nodes.push(ap('core-ap', { name: 'core-ap' }), ap('core-ap2', { name: 'core-ap2' }));
    links.push(link('core', 'core-ap'), link('core', 'core-ap2'), link('sw1-ap00', 'sw2-ap00'));
    const one = layoutGraph({ nodes, links });
    const two = layoutGraph({ nodes: shuffle(nodes, 5), links: shuffle(links, 11) });
    expect(two).toEqual(one);
    const names = byId(one, bundleId('sw1')).bundle!.members.map((m) => m.name);
    expect(names).toEqual([...names].sort());
  });
});

describe('layoutGraph — role rows (ADR-191 Inc.6)', () => {
  it('names the rows top first, in the order the layout draws them', () => {
    // The side panel lists these in place of the sentence that spelled the order out (ADR-200).
    expect(ROW_ORDER).toEqual(['edge', 'l3_switch', 'l2_switch', 'other']);
  });

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
    const bundles = out.nodes.filter((x) => x.ap);
    expect(bundles.map((n) => n.bundle?.members.length)).toEqual([4, 3, 3, 3]);
    for (const n of bundles) expect(n.cy).toBeGreaterThan(at(n.bundle!.parent).cy);
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
      const b = out.nodes.find((n) => n.id === bundleId(sw))!;
      expect(b.cx).toBeCloseTo(parent.cx, 6);
      expect(b.cy).toBeGreaterThan(parent.cy);
    }
  });
});

describe('layoutGraph — a shape a pane can be fitted to (ADR-191 Inc.14)', () => {
  const pad = (i: number) => String(i).padStart(4, '0');
  /** A router with `n` switches straight under it, each with `aps` access points. */
  function star(n: number, aps = 0) {
    const nodes = [node('r', { role: 'edge' })];
    const links: GraphLink[] = [];
    for (let i = 0; i < n; i++) {
      const sw = `sw${pad(i)}`;
      nodes.push(node(sw, { role: 'l2_switch' }));
      links.push(link('r', sw));
      for (let j = 0; j < aps; j++) {
        const id = `${sw}-ap${j}`;
        nodes.push(node(id, { ap: true, role: 'access_point', name: id }));
        links.push(link(sw, id));
      }
    }
    return { nodes, links };
  }
  const rowsOf = (out: ReturnType<typeof layoutGraph>, prefix: string) =>
    new Set(out.nodes.filter((n) => n.id.startsWith(prefix) && !n.ap).map((n) => n.cy)).size;
  function expectNoBoxOverlap(out: ReturnType<typeof layoutGraph>) {
    const rects = out.nodes.map((n) => {
      const w = n.ap ? AP_PITCH : n.w;
      return { id: n.id, l: n.cx - w / 2, r: n.cx + w / 2, t: n.cy - n.h / 2, b: n.cy + n.h / 2 };
    });
    for (let i = 0; i < rects.length; i++) {
      for (let j = i + 1; j < rects.length; j++) {
        const [p, q] = [rects[i], rects[j]];
        expect(p.l < q.r && q.l < p.r && p.t < q.b && q.t < p.b, `${p.id} overlaps ${q.id}`).toBe(false);
      }
    }
  }

  it('wraps a rank into the fewest even rows, keeping the order', () => {
    expect(wrapRow([1, 2, 3], 8)).toEqual([[1, 2, 3]]);
    expect(wrapRow([], 8)).toEqual([[]]);
    expect(wrapRow([1, 2, 3, 4, 5, 6, 7, 8, 9], 8)).toEqual([
      [1, 2, 3, 4, 5],
      [6, 7, 8, 9],
    ]);
    const nineteen = Array.from({ length: 19 }, (_, i) => i);
    expect(wrapRow(nineteen, 8).map((r) => r.length)).toEqual([7, 7, 5]);
    expect(wrapRow(nineteen, 8).flat()).toEqual(nineteen);
  });

  it('caps a row at about the square root of the component, never under the minimum', () => {
    expect(rowCap(1)).toBe(ROW_WRAP_MIN);
    expect(rowCap(20)).toBe(ROW_WRAP_MIN);
    expect(rowCap(400)).toBe(20);
    expect(rowCap(401)).toBe(21);
  });

  it('starts a second band only past the widest component and the minimum', () => {
    expect(shelfCols([{ rows: [[]], width: 3 }])).toBe(SHELF_MIN_COLS);
    expect(shelfCols([{ rows: [[]], width: 40 }])).toBe(40);
    // 100 islands of 2×2: area (2 + 2) × 2 × 100 = 800 ⇒ 29 columns.
    expect(shelfCols(Array.from({ length: 100 }, () => ({ rows: [[], []], width: 2 })))).toBe(29);
  });

  it('keeps a small site exactly as it was', () => {
    const out = layoutGraph(star(ROW_WRAP_MIN, 2));
    expect(rowsOf(out, 'sw')).toBe(1);
  });

  it('draws twenty switches under one router as a block of rows, not one strip', () => {
    const out = layoutGraph(star(20, 3));
    expect(rowsOf(out, 'sw')).toBe(3);
    expectNoBoxOverlap(out);
    for (const n of out.nodes) expect(n.cx + (n.ap ? AP_PITCH : n.w) / 2).toBeLessThanOrEqual(out.width);
    // Narrow enough that a 1280-px pane fits it above the old 0.25 floor.
    expect(out.width * 0.25).toBeLessThan(1280);
    // A line to a switch in a lower row bows rather than crossing the row above.
    const far = out.nodes.filter((n) => n.id.startsWith('sw') && !n.ap).sort((a, b) => b.cy - a.cy)[0];
    const e = out.edges.find((x) => x.id.length > 0 && Math.abs(x.y2 - far.cy) < 1 && Math.abs(x.x2 - far.cx) < 1)!;
    expect(e.kind).toBe('bow');
  });

  it('keeps a fleet-sized star roughly as wide as it is tall', () => {
    const out = layoutGraph(star(1999));
    expect(out.width / out.height).toBeLessThan(4);
    expect(out.height / out.width).toBeLessThan(4);
  });

  it('stacks many islands into bands instead of one line', () => {
    const nodes: GraphNode[] = [];
    const links: GraphLink[] = [];
    for (let i = 0; i < 120; i++) {
      nodes.push(node(`a${pad(i)}`), node(`b${pad(i)}`));
      links.push(link(`a${pad(i)}`, `b${pad(i)}`));
    }
    const out = layoutGraph({ nodes, links });
    expect(out.componentCount).toBe(120);
    expect(out.width / out.height).toBeLessThan(4);
    expectNoBoxOverlap(out);
  });

  it('does not depend on input order once wrapped', () => {
    const { nodes, links } = star(30, 2);
    const a = layoutGraph({ nodes, links });
    const b = layoutGraph({ nodes: shuffle(nodes, 7), links: shuffle(links, 11) });
    expect(b).toEqual(a);
  });
});
