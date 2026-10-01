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
// Since ADR-191 Inc.5 a Wi-Fi access point is not a box in the grid. The grid is laid out from the
// other boxes and the links between them (the backbone); each access point then hangs in a band
// under its parent — the backbone box it is linked to in the highest row — at a narrower pitch,
// wrapping after `AP_PER_LINE`. A column widens to fit its group and a row grows by its bands, so
// a group never overlaps its neighbour's. An island of access points with no backbone box to hang
// from is laid out as backbone, so it is still drawn.
//
// Since ADR-191 Inc.6 the rows follow what each device does, as the server judged it: routers and
// firewalls on top, then switches that route, then switches that do not, and everything else (a
// wireless controller, a server, a folder or stub box, an island of access points) one row below
// the nearest box it is linked to. Inside one of those three bands a box linked to the band above
// starts it, and the band splits into rows by hops from those starts, so a daisy-chained access
// switch sits under the one it hangs off. A component with no router or switch in it at all keeps
// the plain layout below (rooted at its best-connected box), so such a level does not move.
//
// ⚠️ DETERMINISM IS A REQUIREMENT, NOT A NICETY.
// The map re-fetches on a timer. A layout that depends on input order, on a random seed, or on
// iteration-until-converged would reshuffle every cycle and be unusable. So: adjacency lists are
// sorted, components are ordered by (size, lowest id), the barycentre pass runs a fixed number of
// sweeps with an id tiebreak, an access point's parent and its place in the group break ties on
// the id, and nothing here reads a clock or a random number.
//
// The fit-once guard in `TopologyMap.tsx` is the *other* half of that and neither replaces the
// other: the guard stops the viewport being reset under the operator, determinism stops the content
// moving underneath a preserved viewport. Removing either one brings the jumping back.

import type { MapRole, NodeState } from '../../types/api';

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
/** A node box tall enough for a second line: the subfolder a node is filed in, on a level drawn
 *  flat (ADR-191 Inc.2). Still shorter than a folder box, so the grid cell does not grow. */
export const NODE_TALL = { w: BOX_SIZE.node.w, h: 52 };

/** An access point is a circle this wide, with its name underneath (ADR-191 Inc.5). */
export const AP_SIZE = { w: 40, h: 40 };
/** The horizontal pitch of access points under one parent; the name below is cut to fit it. */
export const AP_PITCH = 104;
/** How many access points share one line under their parent before the group wraps. */
export const AP_PER_LINE = 8;
/** One band of access points: the circle, two lines of text under it, and the gap below. */
export const AP_LINE_H = 96;

/** The size one box is drawn at. */
export function boxSize(n: Pick<GraphNode, 'kind' | 'sub' | 'ap'>): { w: number; h: number } {
  if (n.kind === 'node' && n.ap) return AP_SIZE;
  return n.kind === 'node' && n.sub ? NODE_TALL : BOX_SIZE[n.kind];
}

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
  /** A second line under the name (a folder's counts, where a stub leads, the subfolder a node is
   *  filed in on a flat level), or null. */
  sub: string | null;
  /** Upstream root-cause node id (dependency suppression), or null. */
  rootCause: string | null;
  /** A Wi-Fi access point: drawn as a circle under its parent rather than as a box in the grid.
   *  Only a `node` can be one. */
  ap: boolean;
  /** What the node does in the network, which decides its row (ADR-191 Inc.6). A folder or stub
   *  box is `other`. */
  role: MapRole;
}

/** The band a role is drawn in, top first. Everything at `UNTIERED` takes the row under the
 *  nearest box it is linked to instead of a band of its own. */
const TIER_OF_ROLE: Record<MapRole, number> = {
  edge: 0,
  l3_switch: 1,
  l2_switch: 2,
  access_point: 3,
  other: 3,
};
const UNTIERED = 3;

function tierOf(n: GraphNode): number {
  return n.kind === 'node' ? TIER_OF_ROLE[n.role] : UNTIERED;
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

/** The caller's anchor when it is among `ids`, otherwise the box with the most backbone links,
 *  ties broken by the lowest id. `ids` must be sorted and non-empty. */
function anchorOf(ids: string[], badj: Map<string, string[]>, anchorId: string | null): string {
  if (anchorId && ids.includes(anchorId)) return anchorId;
  return ids.reduce((best, id) => {
    const d = (badj.get(id) ?? []).length;
    const bd = (badj.get(best) ?? []).length;
    return d > bd || (d === bd && id < best) ? id : best;
  }, ids[0]);
}

/** Hop distance from `starts` (all at 0), moving only through `within`. */
function hops(starts: string[], within: Set<string>, badj: Map<string, string[]>): Map<string, number> {
  const depth = new Map<string, number>(starts.map((id) => [id, 0]));
  const queue = [...starts];
  while (queue.length > 0) {
    const cur = queue.shift()!;
    const d = depth.get(cur)!;
    for (const next of badj.get(cur) ?? []) {
      if (within.has(next) && !depth.has(next)) {
        depth.set(next, d + 1);
        queue.push(next);
      }
    }
  }
  return depth;
}

/**
 * The row of every box of one connected component (`members`, sorted).
 *
 * With no router or switch in the component this is the plain layout: hop distance from the
 * anchor. Otherwise each role band (edge, L3, L2) gets consecutive rows — a band starts from its
 * boxes linked to a higher band and splits by hops inside itself; an empty band takes no row — and
 * every other box sits one row under the nearest box it is linked to.
 */
function rankComponent(
  members: string[],
  badj: Map<string, string[]>,
  byId: Map<string, GraphNode>,
  anchorId: string | null,
): Map<string, number> {
  const tier = new Map(members.map((id) => [id, tierOf(byId.get(id)!)]));
  if (members.every((id) => tier.get(id) === UNTIERED)) {
    return hops([anchorOf(members, badj, anchorId)], new Set(members), badj);
  }
  const rank = new Map<string, number>();
  let base = 0;
  for (let t = 0; t < UNTIERED; t++) {
    const band = members.filter((id) => tier.get(id) === t);
    if (band.length === 0) continue;
    let starts = band.filter((id) => (badj.get(id) ?? []).some((n) => tier.get(n)! < t));
    if (starts.length === 0) starts = [anchorOf(band, badj, anchorId)];
    const depth = hops(starts, new Set(band), badj);
    let deepest = 0;
    for (const id of band) {
      const d = depth.get(id) ?? 0;
      rank.set(id, base + d);
      deepest = Math.max(deepest, d);
    }
    base += deepest + 1;
  }
  // Everything else: one row under the nearest ranked box, relaxed to a fixed point so a chain of
  // such boxes (a controller behind a server) steps down one row per hop. The fixed point is the
  // shortest distance, so it does not depend on the order the boxes are visited in.
  const rest = members.filter((id) => tier.get(id) === UNTIERED);
  let changed = true;
  while (changed) {
    changed = false;
    for (const id of rest) {
      let best = Infinity;
      for (const n of badj.get(id) ?? []) {
        const r = rank.get(n);
        if (r !== undefined) best = Math.min(best, r + 1);
      }
      if (best < (rank.get(id) ?? Infinity)) {
        rank.set(id, best);
        changed = true;
      }
    }
  }
  return rank;
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

  const isolatedCount = input.nodes.filter((n) => n.kind === 'node' && !adj.has(n.id)).length;
  const looseFolders = input.nodes
    .filter((n) => n.kind === 'folder' && !adj.has(n.id))
    .sort((x, y) => byText(x.name, y.name) || byText(x.id, y.id));

  // 1b. Which access points hang under a parent. An access point linked to no backbone box, even
  //     through other access points, belongs to an island of access points only; there is nothing
  //     to hang it from, so it joins the backbone and is laid out like any box.
  const isAp = (id: string) => {
    const n = byId.get(id)!;
    return n.kind === 'node' && n.ap;
  };
  const hung = new Set<string>();
  const visited = new Set<string>();
  for (const start of [...adj.keys()].sort()) {
    if (!isAp(start) || visited.has(start)) continue;
    const cluster: string[] = [];
    let attached = false;
    const queue = [start];
    visited.add(start);
    while (queue.length > 0) {
      const cur = queue.shift()!;
      cluster.push(cur);
      for (const next of adj.get(cur) ?? []) {
        if (!isAp(next)) {
          attached = true;
        } else if (!visited.has(next)) {
          visited.add(next);
          queue.push(next);
        }
      }
    }
    if (attached) for (const id of cluster) hung.add(id);
  }
  // The backbone: every linked box but the hung access points, and the links between them. A box
  // whose only links go to access points is still a member — it is their parent.
  const badj = new Map<string, string[]>();
  for (const [id, list] of adj) {
    if (!hung.has(id)) badj.set(id, list.filter((n) => !hung.has(n)));
  }
  const linked = [...badj.keys()].sort();

  // 2. Connected components of the backbone, by iterative BFS. Each component is identified by its
  //    lowest id, and components are ordered by (size desc, lowest id) so the biggest island reads
  //    first and the order never depends on which box the input happened to list first.
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
      for (const next of badj.get(cur) ?? []) {
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
    // 3-4. Rows. See the file header: role bands when the component holds a router or switch,
    //      otherwise hop distance from one anchor.
    const rank = rankComponent(members, badj, byId, input.anchorId ?? null);

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
          const neighbours = (badj.get(id) ?? []).filter((n) => (rank.get(n) ?? -1) === r + towards);
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

  // 6c. Each hung access point's parent: of its backbone neighbours, the one in the highest row,
  //     then the lowest id. One linked only to other access points takes the parent of the nearest
  //     one that has its own (fewest hops, then lowest id) — a mesh repeater hangs beside the AP it
  //     repeats. Step 1b moved every island to the backbone, so every hung one finds a parent.
  const parentOf = hangAccessPoints(hung, adj, cell);
  const groups = new Map<string, string[]>();
  for (const [ap, parent] of parentOf) {
    if (!groups.has(parent)) groups.set(parent, []);
    groups.get(parent)!.push(ap);
  }
  for (const list of groups.values()) {
    list.sort((x, y) => byText(byId.get(x)!.name, byId.get(y)!.name) || byText(x, y));
  }

  // 6d. Column widths and row heights. A column is as wide as the widest group hanging in it, and
  //     a row grows by the bands of access points under it. With no access point every column is
  //     CELL_W and every row CELL_H, so a level without one lays out exactly as before.
  let maxCol = 0;
  for (const { col } of cell.values()) maxCol = Math.max(maxCol, col);
  const colContent = new Array<number>(maxCol + 1).fill(BOX_W);
  const apBands = new Array<number>(Math.max(maxRank, 0) + 1).fill(0);
  for (const [parent, list] of groups) {
    const { col, rank } = cell.get(parent)!;
    colContent[col] = Math.max(colContent[col], Math.min(list.length, AP_PER_LINE) * AP_PITCH);
    apBands[rank] = Math.max(apBands[rank], Math.ceil(list.length / AP_PER_LINE));
  }
  const colLeft: number[] = [];
  let x = PAD;
  for (let c = 0; c <= maxCol; c++) {
    colLeft.push(x);
    x += colContent[c] + COL_GAP;
  }
  const rowTop: number[] = [];
  let y = PAD;
  for (let r = 0; r <= maxRank; r++) {
    rowTop.push(y);
    y += CELL_H + apBands[r] * AP_LINE_H;
  }
  const cxOf = (col: number) => colLeft[col] + colContent[col] / 2;
  const cyOf = (rank: number) => rowTop[rank] + BOX_H / 2;

  const placed: PlacedNode[] = [];
  const at = new Map<string, { cx: number; cy: number; h: number }>();
  const put = (n: GraphNode, cx: number, cy: number) => {
    const size = boxSize(n);
    at.set(n.id, { cx, cy, h: size.h });
    placed.push({ ...n, cx, cy, w: size.w, h: size.h, suppressed: n.rootCause != null });
  };
  for (const [id, { col, rank }] of cell) put(byId.get(id)!, cxOf(col), cyOf(rank));
  for (const [parent, list] of groups) {
    const { col, rank } = cell.get(parent)!;
    list.forEach((id, i) => {
      const line = Math.floor(i / AP_PER_LINE);
      const inLine = Math.min(AP_PER_LINE, list.length - line * AP_PER_LINE);
      const k = i % AP_PER_LINE;
      put(
        byId.get(id)!,
        cxOf(col) + (k - (inLine - 1) / 2) * AP_PITCH,
        rowTop[rank] + CELL_H + line * AP_LINE_H + AP_SIZE.h / 2,
      );
    });
  }
  placed.sort((a, b) => byText(a.id, b.id));

  // 7. Edges. A rank-adjacent pair is a straight line from the bottom of one box to the top of the
  //    next. A same-rank or rank-skipping pair bows sideways so it does not run through the boxes
  //    between its ends; the bow's direction and size come from the endpoints' own coordinates, so
  //    it is deterministic. An access point's line to its parent is straight; any other line that
  //    touches a hung access point bows off the straight line between the two centres.
  const edges: PlacedEdge[] = [];
  const keys = [...edgeOf.keys()].sort();
  for (const key of keys) {
    const link = edgeOf.get(key)!;
    const [a, b] = key.split('|');
    const source = link.source || 'l3_subnet';
    const count = link.count;

    if (hung.has(a) || hung.has(b)) {
      const qa = at.get(a)!;
      const qb = at.get(b)!;
      // Draw from the higher end, so the far end is the one whose suppression mutes the line.
      const aFirst = qa.cy < qb.cy || (qa.cy === qb.cy && qa.cx <= qb.cx);
      const [fromId, toId] = aFirst ? [a, b] : [b, a];
      const [from, to] = aFirst ? [qa, qb] : [qb, qa];
      const suppressed = byId.get(toId)?.rootCause != null;
      if (parentOf.get(toId) === fromId) {
        const y1 = from.cy + from.h / 2;
        const y2 = to.cy - to.h / 2;
        const chip = { x: (from.cx + to.cx) / 2, y: (y1 + y2) / 2 };
        edges.push({
          id: link.id,
          x1: from.cx,
          y1,
          x2: to.cx,
          y2,
          kind: 'line',
          source,
          count,
          chip,
          suppressed,
        });
      } else {
        edges.push(bowBelow(link.id, from, to, source, count, suppressed));
      }
      continue;
    }

    const pa = cell.get(a);
    const pb = cell.get(b);
    if (!pa || !pb) continue;
    // Draw from the shallower end so a bow's direction does not depend on which id sorted first.
    const aFirst = pa.rank <= pb.rank;
    const [from, to] = aFirst ? [pa, pb] : [pb, pa];
    const [fromId, toId] = aFirst ? [a, b] : [b, a];
    const hFrom = boxSize(byId.get(fromId)!).h;
    const hTo = boxSize(byId.get(toId)!).h;
    const x1 = cxOf(from.col);
    const x2 = cxOf(to.col);
    const adjacent = to.rank - from.rank === 1;
    const y1 = adjacent ? cyOf(from.rank) + hFrom / 2 : cyOf(from.rank);
    const y2 = adjacent ? cyOf(to.rank) - hTo / 2 : cyOf(to.rank);
    const suppressed = byId.get(toId)?.rootCause != null;

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

  // The last row ends at its boxes, or at the bottom of the last band of access points under it.
  const bottomOf = (r: number) =>
    apBands[r] > 0 ? CELL_H + apBands[r] * AP_LINE_H - (AP_LINE_H - AP_SIZE.h) / 2 : BOX_H;
  const hasNodes = placed.length > 0;
  return {
    nodes: placed,
    edges,
    width: hasNodes ? colLeft[maxCol] + colContent[maxCol] + PAD : 0,
    height: hasNodes ? rowTop[maxRank] + bottomOf(maxRank) + PAD : 0,
    isolatedCount,
    componentCount: components.length,
  };
}

/** Step 6c of `layoutGraph`: which backbone box each hung access point sits under. */
function hangAccessPoints(
  hung: Set<string>,
  adj: Map<string, string[]>,
  cell: Map<string, { col: number; rank: number }>,
): Map<string, string> {
  const direct = new Map<string, string>();
  for (const ap of hung) {
    let best: string | null = null;
    let bestRank = Infinity;
    // `adj` lists are sorted, so the first neighbour at the best rank is the lowest id.
    for (const n of adj.get(ap) ?? []) {
      if (hung.has(n)) continue;
      const r = cell.get(n)!.rank;
      if (r < bestRank) {
        best = n;
        bestRank = r;
      }
    }
    if (best !== null) direct.set(ap, best);
  }
  const parentOf = new Map(direct);
  for (const ap of [...hung].sort()) {
    if (parentOf.has(ap)) continue;
    const reached = new Set([ap]);
    let layer = [ap];
    while (layer.length > 0 && !parentOf.has(ap)) {
      const next: string[] = [];
      for (const cur of layer) {
        for (const n of adj.get(cur) ?? []) {
          if (hung.has(n) && !reached.has(n)) {
            reached.add(n);
            next.push(n);
          }
        }
      }
      next.sort();
      const found = next.find((n) => direct.has(n));
      if (found !== undefined) parentOf.set(ap, direct.get(found)!);
      layer = next;
    }
  }
  return parentOf;
}

/** A line between two points that bows off the straight path, towards the side below it (or to the
 *  right, for a vertical one): a bow between two access points on one line dips under the row
 *  instead of running through the circles between them. */
function bowBelow(
  id: string,
  from: { cx: number; cy: number },
  to: { cx: number; cy: number },
  source: string,
  count: number,
  suppressed: boolean,
): PlacedEdge {
  const dx = to.cx - from.cx;
  const dy = to.cy - from.cy;
  const len = Math.hypot(dx, dy) || 1;
  let nx = -dy / len;
  let ny = dx / len;
  if (ny < 0 || (ny === 0 && nx < 0)) {
    nx = -nx;
    ny = -ny;
  }
  const bow = Math.min(AP_LINE_H, len / 3 + AP_SIZE.h);
  const mx = (from.cx + to.cx) / 2 + nx * bow;
  const my = (from.cy + to.cy) / 2 + ny * bow;
  return {
    id,
    x1: from.cx,
    y1: from.cy,
    x2: to.cx,
    y2: to.cy,
    kind: 'bow',
    path: `M ${from.cx} ${from.cy} Q ${mx} ${my} ${to.cx} ${to.cy}`,
    source,
    count,
    chip: { x: 0.25 * from.cx + 0.5 * mx + 0.25 * to.cx, y: 0.25 * from.cy + 0.5 * my + 0.25 * to.cy },
    suppressed,
  };
}
