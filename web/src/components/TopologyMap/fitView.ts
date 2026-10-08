// SPDX-License-Identifier: AGPL-3.0-only
// The topology map's viewport transform: how a diagram of arbitrary size is fitted into the pane,
// and the zoom bounds every gesture is clamped to.
//
// Extracted from TopologyMap.tsx so the geometry is testable (Vitest never runs `.tsx`). Its
// sibling `layout.ts` — which decides where the nodes go — has been tested all along; this half,
// which decides what the operator actually sees of that layout, had not.

import type { GraphLayout } from './graphLayout';

/** Zoom bounds for a gesture. A level too big to fit at `MIN_SCALE` lowers the floor to its own fit
 *  (`zoomFloor`), so "Fit" always shows the whole level and zooming out never jumps back in. */
export const MIN_SCALE = 0.25;
export const MAX_SCALE = 2.5;
/** The fit itself never goes below this. The layout keeps a level near the pane's shape (ADR-191
 *  Inc.14), so only a level far past the server's 2,000-node cap could reach it. */
export const FIT_FLOOR = 0.02;

/** Fraction of the viewport the fitted diagram fills, leaving a margin so edge nodes are not flush
 *  against the pane border. */
const MARGIN = 0.92;

/** The pan/zoom transform applied to the diagram group. */
export interface View {
  tx: number;
  ty: number;
  scale: number;
}

/** Fit the diagram into the viewport with a little margin, centered.
 *
 *  A zero dimension on either side means there is nothing to fit yet (an empty topology, or a pane
 *  that has not been measured): the identity transform is returned rather than a division by zero,
 *  which would put the diagram at `NaN` and render nothing at all with no error. */
export function fitView(layout: GraphLayout, vw: number, vh: number): View {
  if (layout.width === 0 || layout.height === 0 || vw === 0 || vh === 0) {
    return { tx: 0, ty: 0, scale: 1 };
  }
  const scale = Math.min(
    MAX_SCALE,
    Math.max(FIT_FLOOR, Math.min((vw / layout.width) * MARGIN, (vh / layout.height) * MARGIN)),
  );
  const tx = (vw - layout.width * scale) / 2;
  const ty = (vh - layout.height * scale) / 2;
  return { tx, ty, scale };
}

/** Move the view so the point (`cx`, `cy`) of the diagram sits in the middle of the viewport, at
 *  the scale the operator is already using (ADR-191 Inc.11: stepping through search hits). */
export function centerOn(cx: number, cy: number, vw: number, vh: number, scale: number): View {
  return { tx: vw / 2 - cx * scale, ty: vh / 2 - cy * scale, scale };
}

/** The smallest scale a gesture may reach on a level whose fit is `fitScale`: `MIN_SCALE`, or the fit
 *  when the level only fits below it. Before ADR-191 Inc.14 the fit was clamped to `MIN_SCALE` as
 *  well, so on a big folder "Fit" left both sides of the map outside the pane. */
export function zoomFloor(fitScale: number): number {
  return Math.min(MIN_SCALE, fitScale);
}

/** Clamp a proposed zoom to `[floor, MAX_SCALE]`. */
export function clampScale(scale: number, floor: number = MIN_SCALE): number {
  return Math.min(MAX_SCALE, Math.max(floor, scale));
}
