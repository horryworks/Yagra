// SPDX-License-Identifier: AGPL-3.0-only
// The topology map's viewport transform: how a diagram of arbitrary size is fitted into the pane,
// and the zoom bounds every gesture is clamped to.
//
// Extracted from TopologyMap.tsx so the geometry is testable (Vitest never runs `.tsx`). Its
// sibling `layout.ts` — which decides where the nodes go — has been tested all along; this half,
// which decides what the operator actually sees of that layout, had not.

import type { GraphLayout } from './graphLayout';

/** Zoom bounds. Shared by the initial fit and the pinch/wheel handlers so no gesture can leave the
 *  diagram at a scale the fit could never produce. */
export const MIN_SCALE = 0.25;
export const MAX_SCALE = 2.5;

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
    Math.max(MIN_SCALE, Math.min((vw / layout.width) * MARGIN, (vh / layout.height) * MARGIN)),
  );
  const tx = (vw - layout.width * scale) / 2;
  const ty = (vh - layout.height * scale) / 2;
  return { tx, ty, scale };
}

/** Clamp a proposed zoom to the same bounds the initial fit obeys. */
export function clampScale(scale: number): number {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}
