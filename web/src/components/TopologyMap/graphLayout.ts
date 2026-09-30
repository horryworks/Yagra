// SPDX-License-Identifier: AGPL-3.0-only
// Pure, side-effect-free layout for one level of the network map (ADR-043, ADR-191). Kept out of
// the React component so it unit-tests in the node env (no DOM) — and because Vitest only runs
// `.ts`, so a judgement left in the `.tsx` is a judgement nothing tests.
//
// This replaces the tidy-tree that served the dependency map. That layout assumed a forest of
// single-parent trees, which is what `nodes.parent_id` could express; the connectivity graph is
// undirected and keeps redundant links, so a tree layout cannot draw it at all — two paths to the
// same node is the case the whole feature exists to represent.
//
// Since ADR-191 a level holds three kinds of box — a node, a subfolder drawn as one box, and a
// dashed stub for links leaving the level — each with its own size. Every grid cell fits the
// largest, so boxes of different kinds never overlap.
//
// ⚠️ DETERMINISM IS A REQUIREMENT, NOT A NICETY.
// The map re-fetches on a timer. A layout that depends on input order, on a random seed, or on
// iteration-until-converged would reshuffle every cycle and be unusable. So: adjacency lists are
// sorted, components are ordered by (size, lowest id), the barycentre pass runs a fixed number of
// sweeps with an id tiebreak, and nothing here reads a clock or a random number.
//
// The fit-once guard in `TopologyMap.tsx` is the *other* half of that and neither replaces the
// other: the guard stops the viewport being reset under the operator, determinism stops the content
// moving underneath a preserved viewport. Removing either one brings the jumping back.

import type { NodeState } from '../../types/api';

/** What a box on the map stands for: a node, a subfolder drawn as one box, or a stub for links
 *  that leave the level. */
export const GRAPH_NODE_KINDS = ['node', 'folder', 'external'] as const;
export type GraphNodeKind = (typeof GRAPH_NODE_KINDS)[number];

/** Box size per kind, in px. Exported so the renderer and tests agree on sizing. */
export const BOX_SIZE: Record<GraphNodeKind, { w: number; h: number }> = {
  node: { w: 150, h: 42 },
  folder: { w: 200, h: 76 },
  external: { w: 160, h: 56 },
};
/** The largest box: every grid cell fits it. */
const BOX_W = Math.max(...GRAPH_NODE_KINDS.map((k) => BOX_SIZE[k].w));
const BOX_H = Math.max(...GRAPH_NODE_KINDS.map((k) => BOX_SIZE[k].h));
/** Gap between a box and the next column / row of the grid. */
const COL_GAP = 40;
const ROW_GAP = 46;
/** Outer padding around the whole diagram. */
const PAD = 24;
/** Gutter between two connected components laid out side by side. */
const COMPONENT_GAP = 2;

export const CELL_W = BOX_W + COL_GAP;
export const CELL_H = BOX_H + ROW_GAP;

/** Fixed barycentre sweeps. Two forward passes and two back; more buys almost nothing on the graph
 *  shapes a network produces, and "iterate until stable" would make the result depend on how the
 *  input happened to be ordered. */
const SWEEPS = 4;

/** One box to place. */
export interface GraphNode {
  id: string;
  kind: GraphNodeKind;
  name: string;
  /** The colour it is drawn with: a node's state, a folder's worst member. */
  state: NodeState;
  /** A second line under the name (a folder's counts, where a stub leads), or null. */
  sub: string | null;
  /** Upstream root-cause node id (dependency suppression), or null. */
  rootCause: string | null;
}

/** One line to draw between two boxes. */
export interface GraphLink {
  id: string;
  a: string;
  b: string;
  /** The strongest evidence behind it — what to colour and label it with. */
  source: string;
  /** How many links it bundles. */
  count: number;
}

/** A box placed at a pixel center, ready to render. */
export interface PlacedNode extends GraphNode {
  /** Center coordinates in the SVG's own (untransformed) coordinate space. */
  cx: number;
  cy: number;
  w: number;
  h: number;
  /** This node's alert is suppressed under an upstream cause. */
  suppressed: boolean;
}

/** An edge as pixel endpoints. Rank-adjacent edges are straight lines; everything else bows so it
 *  does not run through the boxes between its ends. */
export interface PlacedEdge {
  id: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** `line` for a rank-adjacent edge, `bow` for a same-rank or rank-skipping one. */
  kind: 'line' | 'bow';
  /** SVG path data, for `bow` edges only. */
  path?: string;
  /** The strongest evidence behind this link — what to colour and label it with. */
  source: string;
  /** How many links it bundles. */
  count: number;
  /** Where the count chip sits: the middle of the line, or of the bow. */
  chip: { x: number; y: number };
  /** The end farther from the anchor is suppressed under an upstream cause — drawn muted. */
  suppressed: boolean;
}

export interface GraphLayout {
  nodes: PlacedNode[];
  edges: PlacedEdge[];
  /** Full diagram size in px (before pan/zoom), so the caller can fit-to-view. */
  width: number;
  height: number;
  /** Nodes with no link at all — shown as a count rather than as a field of loose boxes. A folder
   *  with no link is placed, since its box is the way down a level. */
  isolatedCount: number;
  /** How many connected components the graph has. More than one is normal, not an error: a device
   *  that speaks no discovery protocol, or a site reached over a link nothing reported, is its own
   *  island. */
  componentCount: number;
}

export interface GraphInput {
  nodes: GraphNode[];
  links: GraphLink[];
  /** Where to root the layout. Without one, the highest-degree box stands in, which puts the most
   *  connected thing at the top. */
  anchorId?: string | null;
}

function byText(x: string, y: string): number {
  return x < y ? -1 : x > y ? 1 : 0;
}

/**
 * Lay out one level of the map.
 *
 * The output depends only on the *content* of the input, never on its order — see the file header.
 */
export function layoutGraph(input: GraphInput): GraphLayout {
  const byId = new Map(input.nodes.map((n) => [n.id, n]));

  // 1. Adjacency, with every list sorted. This single sort is what makes every later step
  //    order-independent; without it, BFS visit order would follow the input array.
  const adj = new Map<string, string[]>();
  const edgeOf = new Map<string, GraphLink>();
  for (const link of [...input.links].sort((x, y) => byText(x.id, y.id))) {
    const { a, b } = link;
    if (!a || !b || a === b) continue;
    if (!byId.has(a) || !byId.has(b)) continue;
    const key = a < b ? `${a}|${b}` : `${b}|${a}`;
    if (edgeOf.has(key)) continue;
    edgeOf.set(key, link);
    if (!adj.has(a)) adj.set(a, []);
    if (!adj.has(b)) adj.set(b, []);
    adj.get(a)!.push(b);
    adj.get(b)!.push(a);
  }
  for (const list of adj.values()) list.sort();

  const linked = [...adj.keys()].sort();
  const isolatedCount = input.nodes.filter((n) => n.kind === 'node' && !adj.has(n.id)).length;
  const looseFolders = input.nodes
    .filter((n) => n.kind === 'folder' && !adj.has(n.id))
    .sort((x, y) => byText(x.name, y.name) || byText(x.id, y.id));

  // 2. Connected components, by iterative BFS. Each component is identified by its lowest id, and
  //    components are ordered by (size desc, lowest id) so the biggest island reads first and the
  //    order never depends on which box the input happened to list first.
  const seen = new Set<string>();
  const components: string[][] = [];
  for (const start of linked) {
    if (seen.has(start)) continue;
    const members: string[] = [];
    const queue = [start];
    seen.add(start);
    while (queue.length > 0) {
      const cur = queue.shift()!;
      members.push(cur);
      for (const next of adj.get(cur) ?? []) {
        if (!seen.has(next)) {
          seen.add(next);
          queue.push(next);
        }
      }
    }
    members.sort();
    components.push(members);
  }
  components.sort((x, y) => y.length - x.length || (x[0] < y[0] ? -1 : 1));

  const cell = new Map<string, { col: number; rank: number }>();
  let xOffsetCells = 0;
  let maxRank = -1;

  for (const members of components) {
    // 3. Anchor: the caller's, when it is in this component; otherwise the highest-degree box,
    //    ties broken by the lowest id.
    const anchor =
      input.anchorId && members.includes(input.anchorId)
        ? input.anchorId
        : members.reduce((best, id) => {
            const d = (adj.get(id) ?? []).length;
            const bd = (adj.get(best) ?? []).length;
            return d > bd || (d === bd && id < best) ? id : best;
          }, members[0]);

    // 4. Rank = hop distance from the anchor. Boxes at equal rank share a row, which is where a
    //    redundant path survives: a second route to the same box is two boxes at one rank plus a
    //    same-rank edge, a shape a tree layout cannot express at all.
    const rank = new Map<string, number>([[anchor, 0]]);
    const queue = [anchor];
    while (queue.length > 0) {
      const cur = queue.shift()!;
      const r = rank.get(cur)!;
      for (const next of adj.get(cur) ?? []) {
        if (!rank.has(next)) {
          rank.set(next, r + 1);
          queue.push(next);
        }
      }
    }

    const rows = new Map<number, string[]>();
    for (const id of members) {
      const r = rank.get(id) ?? 0;
      if (!rows.has(r)) rows.set(r, []);
      rows.get(r)!.push(id);
    }
    for (const list of rows.values()) list.sort();
    const ranks = [...rows.keys()].sort((x, y) => x - y);

    // 5. Barycentre ordering: place each box near the average position of its neighbours in the
    //    adjacent rank, a fixed number of times. Classic Sugiyama crossing reduction — cheap, and
    //    deterministic because the sweep count is fixed and ties break on the id.
    const pos = new Map<string, number>();
    for (const r of ranks) rows.get(r)!.forEach((id, i) => pos.set(id, i));
    for (let sweep = 0; sweep < SWEEPS; sweep++) {
      const order = sweep % 2 === 0 ? ranks : [...ranks].reverse();
      const towards = sweep % 2 === 0 ? -1 : 1;
      for (const r of order) {
        const row = rows.get(r)!;
        const bary = new Map<string, number>();
        for (const id of row) {
          const neighbours = (adj.get(id) ?? []).filter((n) => (rank.get(n) ?? -1) === r + towards);
          const sum = neighbours.reduce((acc, n) => acc + (pos.get(n) ?? 0), 0);
          bary.set(id, neighbours.length > 0 ? sum / neighbours.length : (pos.get(id) ?? 0));
        }
        row.sort((x, y) => bary.get(x)! - bary.get(y)! || (x < y ? -1 : 1));
        row.forEach((id, i) => pos.set(id, i));
      }
    }

    // 6. Cells. Each component occupies its own horizontal band, laid out left to right.
    let width = 0;
    for (const r of ranks) {
      const row = rows.get(r)!;
      width = Math.max(width, row.length);
      row.forEach((id, i) => cell.set(id, { col: xOffsetCells + i, rank: r }));
      maxRank = Math.max(maxRank, r);
    }
    xOffsetCells += width + COMPONENT_GAP;
  }

  // 6b. A folder with no link on this level still gets its box — the box is the way down a level.
  //     They go in a grid below everything linked, in name order.
  if (looseFolders.length > 0) {
    const cols = Math.max(4, Math.ceil(Math.sqrt(looseFolders.length)));
    const top = maxRank >= 0 ? maxRank + 2 : 0;
    looseFolders.forEach((f, i) => {
      const rank = top + Math.floor(i / cols);
      cell.set(f.id, { col: i % cols, rank });
      maxRank = Math.max(maxRank, rank);
    });
  }

  const cxOf = (col: number) => PAD + col * CELL_W + BOX_W / 2;
  const cyOf = (rank: number) => PAD + rank * CELL_H + BOX_H / 2;

  const placed: PlacedNode[] = [];
  let maxCol = 0;
  for (const [id, { col, rank }] of cell) {
    const n = byId.get(id)!;
    const size = BOX_SIZE[n.kind];
    maxCol = Math.max(maxCol, col);
    placed.push({
      ...n,
      cx: cxOf(col),
      cy: cyOf(rank),
      w: size.w,
      h: size.h,
      suppressed: n.rootCause != null,
    });
  }
  placed.sort((a, b) => byText(a.id, b.id));

  // 7. Edges. A rank-adjacent pair is a straight line from the bottom of one box to the top of the
  //    next. A same-rank or rank-skipping pair bows sideways so it does not run through the boxes
  //    between its ends; the bow's direction and size come from the endpoints' own coordinates, so
  //    it is deterministic.
  const edges: PlacedEdge[] = [];
  const keys = [...edgeOf.keys()].sort();
  for (const key of keys) {
    const link = edgeOf.get(key)!;
    const [a, b] = key.split('|');
    const pa = cell.get(a);
    const pb = cell.get(b);
    if (!pa || !pb) continue;
    // Draw from the shallower end so a bow's direction does not depend on which id sorted first.
    const aFirst = pa.rank <= pb.rank;
    const [from, to] = aFirst ? [pa, pb] : [pb, pa];
    const [fromId, toId] = aFirst ? [a, b] : [b, a];
    const hFrom = BOX_SIZE[byId.get(fromId)!.kind].h;
    const hTo = BOX_SIZE[byId.get(toId)!.kind].h;
    const x1 = cxOf(from.col);
    const x2 = cxOf(to.col);
    const adjacent = to.rank - from.rank === 1;
    const y1 = adjacent ? cyOf(from.rank) + hFrom / 2 : cyOf(from.rank);
    const y2 = adjacent ? cyOf(to.rank) - hTo / 2 : cyOf(to.rank);
    const source = link.source || 'l3_subnet';
    const suppressed = byId.get(toId)?.rootCause != null;
    const count = link.count;

    if (adjacent) {
      const chip = { x: (x1 + x2) / 2, y: (y1 + y2) / 2 };
      edges.push({ id: link.id, x1, y1, x2, y2, kind: 'line', source, count, chip, suppressed });
    } else {
      // Bow outwards by an amount that grows with the span.
      const span = Math.max(Math.abs(x2 - x1), Math.abs(to.rank - from.rank) * CELL_H);
      const bow = Math.min(CELL_W, span / 3 + BOX_W / 2);
      const dir = x1 <= x2 ? 1 : -1;
      const mx = (x1 + x2) / 2 + dir * bow;
      const my = (y1 + y2) / 2;
      // The point on the quadratic curve at t = 1/2.
      const chip = { x: 0.25 * x1 + 0.5 * mx + 0.25 * x2, y: 0.25 * y1 + 0.5 * my + 0.25 * y2 };
      edges.push({
        id: link.id,
        x1,
        y1,
        x2,
        y2,
        kind: 'bow',
        path: `M ${x1} ${y1} Q ${mx} ${my} ${x2} ${y2}`,
        source,
        count,
        chip,
        suppressed,
      });
    }
  }

  const hasNodes = placed.length > 0;
  return {
    nodes: placed,
    edges,
    width: hasNodes ? PAD * 2 + maxCol * CELL_W + BOX_W : 0,
    height: hasNodes ? PAD * 2 + maxRank * CELL_H + BOX_H : 0,
    isolatedCount,
    componentCount: components.length,
  };
}
