// SPDX-License-Identifier: AGPL-3.0-only
// Pure, side-effect-free tidy-tree layout for the dependency map. Kept out of the React
// component so it unit-tests in the node env (no DOM). The dependency graph is a forest of
// single-parent trees (yagra-common Node.parent is Option<NodeId>), so a classic post-order
// tidy layout — leaves take sequential columns, a parent centers over its children's span —
// gives a clean, non-overlapping map without pulling in a graph-layout dependency.

import type { TopologyNode } from '../../types/api';
import { buildForest, type TopoTreeNode } from '../../dashboard/widgets/util';

/** Grid geometry (in cells) → pixels. Exported so the renderer and tests agree on sizing. */
export const NODE_W = 150;
export const NODE_H = 42;
/** Gap between a node box and the next column / row of the grid. */
const COL_GAP = 40;
const ROW_GAP = 46;
/** Outer padding around the whole diagram. */
const PAD = 24;

const CELL_W = NODE_W + COL_GAP;
const CELL_H = NODE_H + ROW_GAP;

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

/** A parent→child dependency edge, as pixel endpoints (parent bottom → child top). */
export interface PlacedEdge {
  id: string;
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** The child end is suppressed under an upstream cause — drawn muted/dashed. */
  suppressed: boolean;
}

export interface TopologyLayout {
  nodes: PlacedNode[];
  edges: PlacedEdge[];
  /** Full diagram size in px (before pan/zoom), so the caller can fit-to-view. */
  width: number;
  height: number;
  /** Count of nodes with no dependency links (parentless leaves) — shown but not mapped. */
  isolatedCount: number;
}

/** True when a forest node participates in the dependency structure (has a parent, i.e. it's a
 *  child in the forest, or has children of its own). A parentless-and-childless node is isolated. */
function isLinked(t: TopoTreeNode): boolean {
  return t.children.length > 0 || t.node.parent_id != null;
}

/**
 * Lay out the dependency forest. Isolated nodes (no parent, no children) are excluded from the
 * diagram and reported via `isolatedCount` so a flat inventory doesn't render thousands of
 * disconnected boxes. Column assignment is a single running counter across all mapped roots, so
 * sibling trees sit side by side without overlap (one empty column between adjacent trees).
 */
export function layoutTopology(nodes: TopologyNode[]): TopologyLayout {
  const forest = buildForest(nodes);
  const mappedRoots = forest.filter(isLinked);
  const isolatedCount = forest.length - mappedRoots.length;

  const placed: PlacedNode[] = [];
  const col = new Map<string, number>(); // node id → grid column (leaf order / centered)
  const depthOf = new Map<string, number>();
  let nextLeafCol = 0;
  let maxDepth = 0;
  let maxCol = 0; // largest column any node occupies (always a leaf — parents are midpoints)

  // Post-order: assign each leaf the next free column; each parent the midpoint of its children.
  // The forest builder guarantees each node appears once (cycle-safe), but cap depth as a belt-
  // and-braces guard so a pathological input can't recurse without bound.
  const MAX_DEPTH = 64;
  const walk = (t: TopoTreeNode, depth: number): number => {
    depthOf.set(t.node.id, depth);
    if (depth > maxDepth) maxDepth = depth;
    let c: number;
    if (t.children.length === 0 || depth >= MAX_DEPTH) {
      c = nextLeafCol;
      if (c > maxCol) maxCol = c;
      nextLeafCol += 1;
    } else {
      const first = walk(t.children[0], depth + 1);
      let last = first;
      for (let i = 1; i < t.children.length; i += 1) {
        last = walk(t.children[i], depth + 1);
      }
      c = (first + last) / 2;
    }
    col.set(t.node.id, c);
    return c;
  };
  for (const root of mappedRoots) {
    walk(root, 0);
    nextLeafCol += 1; // one blank column between adjacent trees
  }

  // Grid cell → pixel center.
  const cxOf = (c: number) => PAD + c * CELL_W + NODE_W / 2;
  const cyOf = (d: number) => PAD + d * CELL_H + NODE_H / 2;

  const byId = new Map(nodes.map((n) => [n.id, n]));
  for (const [id, c] of col) {
    const n = byId.get(id)!;
    placed.push({
      id,
      name: n.name,
      state: n.state,
      cx: cxOf(c),
      cy: cyOf(depthOf.get(id) ?? 0),
      rootCause: n.root_cause,
      suppressed: n.root_cause != null,
    });
  }

  const placedById = new Map(placed.map((p) => [p.id, p]));
  const edges: PlacedEdge[] = [];
  for (const p of placed) {
    const parentId = byId.get(p.id)!.parent_id;
    if (!parentId) continue;
    const parent = placedById.get(parentId);
    if (!parent) continue; // parent filtered out (shouldn't happen for a linked child) — skip
    edges.push({
      id: `${parentId}->${p.id}`,
      x1: parent.cx,
      y1: parent.cy + NODE_H / 2,
      x2: p.cx,
      y2: p.cy - NODE_H / 2,
      suppressed: p.suppressed,
    });
  }

  const hasNodes = placed.length > 0;
  const width = hasNodes ? PAD * 2 + maxCol * CELL_W + NODE_W : 0;
  const height = hasNodes ? PAD * 2 + maxDepth * CELL_H + NODE_H : 0;

  return { nodes: placed, edges, width, height, isolatedCount };
}
