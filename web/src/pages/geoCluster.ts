// SPDX-License-Identifier: AGPL-3.0-only
// Which Geo map pins are drawn as one (ADR-206). A pin keeps the same size on screen at every zoom,
// so whether two pins overlap is a question about screen distance — map distance times the scale.
// Sites that share a coordinate never separate however far the map is zoomed, so past a point the
// only way to make each one reachable is to draw them as one marker that lists them.
//
// A `.ts` file because Vitest never runs `.tsx`; the page only draws what this returns.

import { SEVERITY_ORDER, emptyStateCounts } from '../lib/nodeState';
import { countsTotal, pinLook, type StateCounts } from '../dashboard/widgets/util';

/** Pins whose centres are closer than this on screen are drawn as one. A pin's halo has a radius
 *  of about 9.5 px, so this is roughly where two of them visibly run into each other. */
export const CLUSTER_PX = 14;

/** One placed site, already projected into map coordinates. */
export interface PinPoint {
  id: string;
  name: string;
  x: number;
  y: number;
  counts: StateCounts | undefined;
}

/** What the map draws at one spot: a single site, or several that overlap at this scale. */
export interface PinCluster {
  /** Stable while the membership is: the first member's id. */
  key: string;
  /** The mean of the members' positions, in map coordinates. */
  x: number;
  y: number;
  /** Worst first (see `worstFirst`). */
  members: PinPoint[];
  /** The members' counts added up, or `undefined` when none of them has any. */
  counts: StateCounts | undefined;
}

/**
 * Group pins that overlap on screen at `scale`.
 *
 * Each pin joins the first group whose **first** member is within `thresholdPx` of it, or starts a
 * new one. Measuring against the first member rather than any member is what keeps it from
 * chaining: A near B and B near C does not pull a far-away C in with A. The order is the input's,
 * so the same pins at the same scale always group the same way — the page re-runs this only when
 * the scale changes, and panning never moves a pin from one group to another.
 */
export function clusterPins(
  points: readonly PinPoint[],
  scale: number,
  thresholdPx = CLUSTER_PX,
): PinCluster[] {
  // In map units, so the comparison does not multiply every distance by the scale.
  const reach = scale > 0 ? thresholdPx / scale : 0;
  const reach2 = reach * reach;
  const groups: PinPoint[][] = [];
  for (const p of points) {
    const home = groups.find((g) => {
      const dx = g[0].x - p.x;
      const dy = g[0].y - p.y;
      return dx * dx + dy * dy < reach2;
    });
    if (home) home.push(p);
    else groups.push([p]);
  }
  return groups.map((members) => ({
    key: members[0].id,
    x: members.reduce((s, m) => s + m.x, 0) / members.length,
    y: members.reduce((s, m) => s + m.y, 0) / members.length,
    members: worstFirst(members),
    counts: sumCounts(members.map((m) => m.counts)),
  }));
}

/** Add tallies together; `undefined` when there was nothing to add, so `pinLook` answers `empty`
 *  for a group of sites that monitor nothing, exactly as it does for one such site. */
export function sumCounts(all: readonly (StateCounts | undefined)[]): StateCounts | undefined {
  const present = all.filter((c): c is StateCounts => c !== undefined);
  if (present.length === 0) return undefined;
  const out = emptyStateCounts();
  for (const c of present) for (const s of SEVERITY_ORDER) out[s] += c[s] ?? 0;
  return out;
}

/** Rank of a pin's look: its state's place in `SEVERITY_ORDER`, with a site that has no nodes last. */
function lookRank(c: StateCounts | undefined): number {
  const look = pinLook(c);
  return look === 'empty' ? SEVERITY_ORDER.length : SEVERITY_ORDER.indexOf(look);
}

/** Sites in the order an operator should look at them: worst state first, then the bigger site,
 *  then by name so the list does not reshuffle between refreshes. */
export function worstFirst(members: readonly PinPoint[]): PinPoint[] {
  return [...members].sort(
    (a, b) =>
      lookRank(a.counts) - lookRank(b.counts) ||
      (b.counts ? countsTotal(b.counts) : 0) - (a.counts ? countsTotal(a.counts) : 0) ||
      a.name.localeCompare(b.name),
  );
}

/** Whether zooming in can ever pull these sites apart. Sites at one coordinate never separate, so
 *  for them a "Zoom in" would only move the map and leave the same marker. */
export function separable(members: readonly PinPoint[]): boolean {
  return members.some((m) => m.x !== members[0].x || m.y !== members[0].y);
}
