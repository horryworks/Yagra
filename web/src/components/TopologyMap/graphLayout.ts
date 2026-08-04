// SPDX-License-Identifier: AGPL-3.0-only
// Pure, side-effect-free layout for the derived connectivity graph (ADR-043). Kept out of the React
// component so it unit-tests in the node env (no DOM) — and because Vitest only runs `.ts`, so a
// judgement left in the `.tsx` is a judgement nothing tests.
//
// This replaces the tidy-tree that served the dependency map. That layout assumed a forest of
// single-parent trees, which is what `nodes.parent_id` could express; the connectivity graph is
// undirected and keeps redundant links, so a tree layout cannot draw it at all — two paths to the
// same node is the case the whole feature exists to represent.
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

import type { TopologyLink, TopologyNode } from '../../types/api';

/** Grid geometry (in cells) → pixels. Exported so the renderer and tests agree on sizing. */
export const NODE_W = 150;
export const NODE_H = 42;
/** Gap between a node box and the next column / row of the grid. */
const COL_GAP = 40;
const ROW_GAP = 46;
/** Outer padding around the whole diagram. */
const PAD = 24;
/** Gutter between two connected components laid out side by side. */
const COMPONENT_GAP = 2;

const CELL_W = NODE_W + COL_GAP;
const CELL_H = NODE_H + ROW_GAP;

/** Fixed barycentre sweeps. Two forward passes and two back; more buys almost nothing on the graph
 *  shapes a network produces, and "iterate until stable" would make the result depend on how the
 *  input happened to be ordered. */
const SWEEPS = 4;

/** Above this many nodes or links the page refuses to draw rather than freezing the browser. */
export const MAX_GRAPH_NODES = 2000;
export const MAX_GRAPH_LINKS = 4000;

/** A node placed at a pixel center, ready to render. */
export interface PlacedNode {
  id: string;
  name: string;
  state: TopologyNode['state'];
  /** Center coordinates in the SVG's own (untransformed) coordinate space. */
  cx: number;
  cy: number;
  /** Upstream root-cause node id (dependency suppression), or null. */
  rootCause: string | null;
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
  /** The end nearer the anchor is suppressed under an upstream cause — drawn muted. */
  suppressed: boolean;
}

export interface GraphLayout {
  nodes: PlacedNode[];
  edges: PlacedEdge[];
  /** Full diagram size in px (before pan/zoom), so the caller can fit-to-view. */
  width: number;
  height: number;
  /** Nodes with no link at all — shown as a count rather than as a field of loose boxes. */
  isolatedCount: number;
  /** How many connected components the graph has. More than one is normal, not an error: a device
   *  that speaks no discovery protocol, or a site reached over a link nothing reported, is its own
   *  island. */
  componentCount: number;
}

export interface GraphInput {
  nodes: TopologyNode[];
  links: TopologyLink[];
  /** Where to root the layout. Increment 2 passes the poller-derived anchor here; until then the
   *  highest-degree node stands in, which puts the most connected device at the top. */
  anchorId?: string | null;
}

/** Both endpoints of a link, or null when either end is not a node in this graph. */
function endsOf(link: TopologyLink): [string, string] | null {
  const a = link.a_node;
  const b = link.b_node;
  if (!a || !b || a === b) return null;
  return [a, b];
}

/**
 * Lay out the connectivity graph.
 *
 * The output depends only on the *content* of the input, never on its order — see the file header.
 */
export function layoutGraph(input: GraphInput): GraphLayout {
  const byId = new Map(input.nodes.map((n) => [n.id, n]));

  // 1. Adjacency, with every list sorted. This single sort is what makes every later step
  //    order-independent; without it, BFS visit order would follow the input array.
  const adj = new Map<string, string[]>();
  const edgeOf = new Map<string, TopologyLink>();
  for (const link of input.links) {
    const ends = endsOf(link);
    if (!ends) continue;
    const [a, b] = ends;
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
  const isolatedCount = input.nodes.length - linked.length;

  // 2. Connected components, by iterative BFS. Each component is identified by its lowest node id,
  //    and components are ordered by (size desc, lowest id) so the biggest island reads first and
  //    the order never depends on which node the input happened to list first.
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

  const placed: PlacedNode[] = [];
  const cell = new Map<string, { col: number; rank: number }>();
  let xOffsetCells = 0;
  let maxRank = 0;

  for (const members of components) {
    // 3. Anchor: the caller's, when it is in this component; otherwise the highest-degree node,
    //    ties broken by the lowest id.
    const anchor =
      input.anchorId && members.includes(input.anchorId)
        ? input.anchorId
        : members.reduce((best, id) => {
            const d = (adj.get(id) ?? []).length;
            const bd = (adj.get(best) ?? []).length;
            return d > bd || (d === bd && id < best) ? id : best;
          }, members[0]);

    // 4. Rank = hop distance from the anchor. Nodes at equal rank share a row, which is where a
    //    redundant path survives: a second route to the same node is two nodes at one rank plus a
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

    // 5. Barycentre ordering: place each node near the average position of its neighbours in the
    //    adjacent rank, a fixed number of times. Classic Sugiyama crossing reduction — cheap, and
    //    deterministic because the sweep count is fixed and ties break on the node id.
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

  const cxOf = (col: number) => PAD + col * CELL_W + NODE_W / 2;
  const cyOf = (rank: number) => PAD + rank * CELL_H + NODE_H / 2;

  let maxCol = 0;
  for (const [id, { col, rank }] of cell) {
    const n = byId.get(id)!;
    maxCol = Math.max(maxCol, col);
    placed.push({
      id,
      name: n.name,
      state: n.state,
      cx: cxOf(col),
      cy: cyOf(rank),
      rootCause: n.root_cause ?? null,
      suppressed: n.root_cause != null,
    });
  }
  placed.sort((a, b) => (a.id < b.id ? -1 : 1));

  // 7. Edges. A rank-adjacent pair is a straight line, exactly as before. A same-rank or
  //    rank-skipping pair bows sideways so it does not run through the boxes between its ends; the
  //    bow's direction and size come from the endpoints' own coordinates, so it is deterministic.
  const edges: PlacedEdge[] = [];
  const keys = [...edgeOf.keys()].sort();
  for (const key of keys) {
    const link = edgeOf.get(key)!;
    const [a, b] = key.split('|');
    const pa = cell.get(a);
    const pb = cell.get(b);
    if (!pa || !pb) continue;
    // Draw from the shallower end so a bow's direction does not depend on which id sorted first.
    const [from, to] = pa.rank <= pb.rank ? [pa, pb] : [pb, pa];
    const toId = pa.rank <= pb.rank ? b : a;
    const x1 = cxOf(from.col);
    const x2 = cxOf(to.col);
    const adjacent = to.rank - from.rank === 1;
    const y1 = adjacent ? cyOf(from.rank) + NODE_H / 2 : cyOf(from.rank);
    const y2 = adjacent ? cyOf(to.rank) - NODE_H / 2 : cyOf(to.rank);
    const source = link.source ?? 'l3_subnet';
    const suppressed = byId.get(toId)?.root_cause != null;

    if (adjacent) {
      edges.push({ id: key, x1, y1, x2, y2, kind: 'line', source, suppressed });
    } else {
      // Bow outwards, away from the diagram's centre column, by an amount that grows with the span.
      const span = Math.max(Math.abs(x2 - x1), Math.abs(to.rank - from.rank) * CELL_H);
      const bow = Math.min(CELL_W, span / 3 + NODE_W / 2);
      const dir = x1 <= x2 ? 1 : -1;
      const mx = (x1 + x2) / 2 + dir * bow;
      const my = (y1 + y2) / 2;
      edges.push({
        id: key,
        x1,
        y1,
        x2,
        y2,
        kind: 'bow',
        path: `M ${x1} ${y1} Q ${mx} ${my} ${x2} ${y2}`,
        source,
        suppressed,
      });
    }
  }

  const hasNodes = placed.length > 0;
  return {
    nodes: placed,
    edges,
    width: hasNodes ? PAD * 2 + maxCol * CELL_W + NODE_W : 0,
    height: hasNodes ? PAD * 2 + maxRank * CELL_H + NODE_H : 0,
    isolatedCount,
    componentCount: components.length,
  };
}
