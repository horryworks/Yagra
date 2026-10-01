// SPDX-License-Identifier: AGPL-3.0-only
// Searching one level of the network map by host name (ADR-191 Inc.11): which boxes a condition
// picks out, how many of a bundle's access points it picks, how many hits are not drawn at all,
// and the order Enter steps through them in. The condition is the column filter's own
// (`lib/filterCondition.ts`), so a substring, a regular expression and NOT read here exactly as
// they do on every list.

import type { MapLevel } from '../../types/api';
import { compileCondition, type TextCondition } from '../../lib/filterCondition';
import type { GraphLayout } from './graphLayout';
import { graphId } from './topologyLevel';

export interface MapSearch {
  /** The boxes the condition picks out: a node whose name matches, or a bundle holding at least one
   *  such access point. Null when no condition is active, which means "draw everything as usual". */
  matched: ReadonlySet<string> | null;
  /** For each bundle with a hit, how many of its access points match. */
  bundleHits: ReadonlyMap<string, number>;
  /** How many of the level's nodes match, drawn or not. */
  total: number;
  /** How many of those have no line, so are counted on the level but not drawn. */
  undrawn: number;
  /** The matched boxes top to bottom, then left to right: the order Enter steps through. */
  order: string[];
}

const NO_SEARCH: MapSearch = { matched: null, bundleHits: new Map(), total: 0, undrawn: 0, order: [] };

/** Run `cond` over the level's node names and map the hits onto the drawn boxes. */
export function searchMap(level: MapLevel | null, layout: GraphLayout, cond: TextCondition): MapSearch {
  const test = compileCondition(cond);
  if (!test || !level) return NO_SEARCH;
  const hits = new Set(level.nodes.filter((n) => test([n.name])).map((n) => graphId('node', n.id)));
  const matched = new Set<string>();
  const bundleHits = new Map<string, number>();
  let drawn = 0;
  for (const box of layout.nodes) {
    if (box.bundle) {
      const n = box.bundle.members.filter((m) => hits.has(m.id)).length;
      if (n > 0) {
        matched.add(box.id);
        bundleHits.set(box.id, n);
        drawn += n;
      }
    } else if (hits.has(box.id)) {
      matched.add(box.id);
      drawn += 1;
    }
  }
  const order = layout.nodes
    .filter((b) => matched.has(b.id))
    .sort((a, b) => a.cy - b.cy || a.cx - b.cx || (a.id < b.id ? -1 : 1))
    .map((b) => b.id);
  return { matched, bundleHits, total: hits.size, undrawn: hits.size - drawn, order };
}

/** The two box ids a drawn line joins. A line the layout drew for a bundle carries a prefix; every
 *  other line is the server's, spelled `<kind>:<id>|<kind>:<id>` (the same spelling as box ids). */
export function edgeEnds(edgeId: string): [string, string] {
  const body = edgeId.startsWith('apgroup-edge:') ? edgeId.slice('apgroup-edge:'.length) : edgeId;
  // A bundle's id is `apgroup:<kind>:<id>`, so split on the bar, never on a colon.
  const [a = '', b = ''] = body.split('|');
  return [a, b];
}

/** How a box is drawn under a search: picked out, faded, or as usual when nothing is searched. */
export function boxEmphasis(id: string, matched: ReadonlySet<string> | null): 'match' | 'dim' | null {
  if (!matched) return null;
  return matched.has(id) ? 'match' : 'dim';
}

/** A line is faded when neither end is picked out. */
export function edgeDimmed(edgeId: string, matched: ReadonlySet<string> | null): boolean {
  if (!matched) return false;
  const [a, b] = edgeEnds(edgeId);
  return !matched.has(a) && !matched.has(b);
}

/** The next box Enter centres on: one step forward (or back) from `current` in `order`, wrapping;
 *  the first (or last) when `current` is not among them. */
export function stepThrough(order: readonly string[], current: string | null, dir: 1 | -1): string | null {
  if (order.length === 0) return null;
  const at = current === null ? -1 : order.indexOf(current);
  if (at < 0) return dir === 1 ? order[0] : order[order.length - 1];
  return order[(at + dir + order.length) % order.length];
}


