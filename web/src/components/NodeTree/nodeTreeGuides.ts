// SPDX-License-Identifier: AGPL-3.0-only
// The tree's branch lines, and what they highlight (ADR-171).
//
// Indentation alone made a deep or long folder impossible to follow — which row hangs off which
// parent was a matter of lining up whitespace. Every row now carries one guide per ancestor level:
// a `│` where an ancestor still has siblings below, and its own `├` / `└` where it joins its parent.
// On top of that: the row the pane shows lights its branch up to the root, the working set lights
// its branches faintly, a folder says how many of the working set are inside it, and the folders
// scrolled away above the viewport stay pinned at the top.
//
// Everything that DECIDES lives here, because Vitest never loads a `.tsx` (testing.md). The tree
// only draws what these return.

import type { FlatRow } from '../../lib/nodeTree';
import type { NodeGroup } from '../../types/api';

/** One guide cell: a pass-through line, a tee (more siblings below), an elbow (last child), or
 *  nothing (the ancestor at this level has no sibling below). */
export type GuideKind = 'pipe' | 'tee' | 'elbow' | null;

/** The depth a row is drawn at. The Ungrouped header has none of its own: it stands at the top
 *  level, like a root folder, and its nodes hang one level below it. */
export function rowDepth(row: FlatRow): number {
  return row.kind === 'ungrouped-head' ? 0 : row.depth;
}

/** A row that other rows hang under: a folder, or the Ungrouped header. */
function isParentRow(row: FlatRow): boolean {
  return row.kind === 'group' || row.kind === 'ungrouped-head';
}

/**
 * The guide cells of every row: `guides[i][k]` is column `k` of row `i`, for `k < depth`.
 *
 * One pass from the bottom. `more[d]` says "a row at depth `d` appears further down under the same
 * parent". A row at depth `d` joins with `├` when that is true and `└` when it is not; each column
 * above that is a `│` exactly when the ancestor one level deeper still has a sibling below.
 * Passing a row resets every deeper level, because anything deeper above it hangs off another
 * parent. Cost: rows × depth.
 */
export function treeGuides(rows: readonly FlatRow[]): GuideKind[][] {
  const out: GuideKind[][] = new Array(rows.length);
  const more: boolean[] = [];
  for (let i = rows.length - 1; i >= 0; i--) {
    const d = rowDepth(rows[i]);
    const cols: GuideKind[] = new Array(d);
    for (let k = 0; k < d - 1; k++) cols[k] = more[k + 1] ? 'pipe' : null;
    if (d > 0) cols[d - 1] = more[d] ? 'tee' : 'elbow';
    out[i] = cols;
    more[d] = true;
    more.length = d + 1;
  }
  return out;
}

/** The index of each row's parent row (a folder or the Ungrouped header), or -1 at the top level.
 *  One pass from the top with a stack of the latest parent row seen at each depth. */
export function parentRows(rows: readonly FlatRow[]): number[] {
  const parent = new Array<number>(rows.length);
  const stack: number[] = [];
  rows.forEach((row, i) => {
    const d = rowDepth(row);
    stack.length = d;
    parent[i] = d > 0 && stack[d - 1] !== undefined ? stack[d - 1] : -1;
    if (isParentRow(row)) stack[d] = i;
  });
  return parent;
}

/** How one guide cell is lit. `up` / `down` are the halves of the vertical line above and below the
 *  row's middle; `stub` is the short horizontal line into the row. */
export interface LitPart {
  up: boolean;
  down: boolean;
  stub: boolean;
}

/** A lit cell, per layer: `strong` is the branch of the row the pane shows, `soft` the branches of
 *  the working set. Both can light the same cell, and the draw decides which colour wins. */
export interface LitCell {
  strong: LitPart;
  soft: LitPart;
}

/** Row index → column → how that cell is lit. Rows and columns not in it are unlit. */
export type LitGuides = Map<number, Map<number, LitCell>>;

function cell(lit: LitGuides, row: number, col: number): LitCell {
  let cols = lit.get(row);
  if (!cols) lit.set(row, (cols = new Map()));
  let c = cols.get(col);
  if (!c) {
    c = {
      strong: { up: false, down: false, stub: false },
      soft: { up: false, down: false, stub: false },
    };
    cols.set(col, c);
  }
  return c;
}

/**
 * Light the branches from each of `starts` up to the root, in one layer.
 *
 * 🚨 **Linear in the rows, not in starts × rows.** The obvious version walks each start up to the
 * root and lights every row between a child and its parent — which, for a folder of 3,000 nodes all
 * in the working set, lights the same rows 3,000 times. Here each parent is climbed once: it
 * remembers the lowest child any branch came from, and the line from the parent down to that child
 * is drawn once. Branches from different folders meet at their common ancestor and share that
 * line from there up, which is what keeps them from crossing or cancelling each other.
 */
function lightLayer(
  lit: LitGuides,
  rows: readonly FlatRow[],
  parent: readonly number[],
  starts: Iterable<number>,
  layer: keyof LitCell,
): void {
  /** Parent row → the lowest child row a branch arrived from. */
  const lowest = new Map<number, number>();
  const joined = new Set<number>();
  for (const start of starts) {
    let r = start;
    while (r >= 0 && rowDepth(rows[r]) > 0 && !joined.has(r)) {
      joined.add(r);
      const p = parent[r];
      if (p < 0) break;
      const seen = lowest.get(p);
      if (seen === undefined || r > seen) lowest.set(p, r);
      // Once a parent is known, the branch above it has already been climbed.
      if (seen !== undefined) break;
      r = p;
    }
  }
  for (const r of joined) {
    const part = cell(lit, r, rowDepth(rows[r]) - 1)[layer];
    part.up = true;
    part.stub = true;
  }
  for (const [p, last] of lowest) {
    const col = rowDepth(rows[p]);
    for (let i = p + 1; i < last; i++) {
      const part = cell(lit, i, col)[layer];
      part.up = true;
      part.down = true;
    }
  }
}

/**
 * Which guide cells to light: the strong branch of `selected` (the row the pane shows, or -1), and
 * the soft branches of `checked` (row indices of the working set). Either may be empty.
 */
export function litGuides(
  rows: readonly FlatRow[],
  parent: readonly number[],
  selected: number,
  checked: Iterable<number>,
): LitGuides {
  const lit: LitGuides = new Map();
  lightLayer(lit, rows, parent, checked, 'soft');
  if (selected >= 0) lightLayer(lit, rows, parent, [selected], 'strong');
  return lit;
}

/** How one part of a guide is drawn: the selected branch, a working-set branch, or plain. */
export type GuideTone = 'strong' | 'soft' | 'plain';

/** The tone of each part of a cell. The pane's branch wins over the working set's where both pass —
 *  it is the one row the operator is reading, and it must stay visible inside a lit batch. */
export function cellTones(c: LitCell | undefined): {
  up: GuideTone;
  down: GuideTone;
  stub: GuideTone;
} {
  const tone = (part: keyof LitPart): GuideTone =>
    !c ? 'plain' : c.strong[part] ? 'strong' : c.soft[part] ? 'soft' : 'plain';
  return { up: tone('up'), down: tone('down'), stub: tone('stub') };
}

/** Row indices of the nodes in the working set, in the order they are drawn. */
export function checkedRowIndices(
  rows: readonly FlatRow[],
  checked: ReadonlyMap<string, unknown>,
): number[] {
  if (checked.size === 0) return [];
  const out: number[] = [];
  rows.forEach((row, i) => {
    if ((row.kind === 'node' || row.kind === 'ungrouped-node') && checked.has(row.node.id)) {
      out.push(i);
    }
  });
  return out;
}

/**
 * How many nodes of the working set sit anywhere under each folder — the "N selected" mark on the
 * folder row (ADR-171 決定 4). A closed folder hides its rows, so the branch lines cannot say that
 * something inside is picked; this can. Computed from the folders, not from the drawn rows, for
 * exactly that reason.
 *
 * ⚠️ A folder chain with a cycle (which the API refuses, but a half-loaded list could show) stops
 * at the first folder it revisits rather than looping.
 */
export function checkedPerGroup(
  checked: ReadonlyMap<string, { group_id?: string | null }>,
  groups: readonly Pick<NodeGroup, 'id' | 'parent_id'>[],
): Map<string, number> {
  const out = new Map<string, number>();
  if (checked.size === 0) return out;
  const parentOf = new Map(groups.map((g) => [g.id, g.parent_id ?? null]));
  for (const node of checked.values()) {
    const seen = new Set<string>();
    let g = node.group_id ?? null;
    while (g && !seen.has(g)) {
      seen.add(g);
      out.set(g, (out.get(g) ?? 0) + 1);
      g = parentOf.get(g) ?? null;
    }
  }
  return out;
}

/** The most folders the pinned band holds (ADR-171 決定 5). Deeper ones win. */
export const STICKY_MAX = 3;

/**
 * The folders to pin at the top of the tree for a given scroll position (ADR-171 決定 5): the
 * parent rows of the row that sits just below the band, which have themselves scrolled under it.
 *
 * The band covers rows, so which row is "below the band" depends on how tall the band is — and the
 * band's height depends on that row. 🚨 **Iterating to a fixed point does not settle**: where a
 * folder ends just under the band, a band of one names a row outside that folder, and a band of
 * none names a row inside it, forever. So instead: the tallest band of `k ≤ max` rows for which
 * the row just below it has at least `k` folders above it that have already scrolled off the top.
 * Those `k` folders, deepest first kept, are the band. A folder still on screen is never pinned —
 * its own row says where the operator is.
 */
export function stickyParents(
  rows: readonly FlatRow[],
  parent: readonly number[],
  scrollTop: number,
  rowHeight: number,
  max: number = STICKY_MAX,
): number[] {
  if (rows.length === 0 || scrollTop <= 0) return [];
  const top = Math.floor(scrollTop / rowHeight);
  for (let k = max; k >= 1; k--) {
    const under = top + k;
    if (under >= rows.length) continue;
    const gone: number[] = [];
    for (let p = parent[under]; p >= 0; p = parent[p]) {
      if (p < top) gone.unshift(p);
    }
    if (gone.length >= k) return gone.slice(-k);
  }
  return [];
}
