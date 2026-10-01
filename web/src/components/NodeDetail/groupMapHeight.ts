// SPDX-License-Identifier: AGPL-3.0-only
// How tall the network map in a folder's pane is, and what a drag of its handle does to it
// (ADR-191 Inc.12). The fifth resize handle, the same shape as the other four (ADR-074):
// `pages/mapPaneHeight.ts` is the one copied.
//
// A `.ts` module because the clamping is the part that can be silently wrong — and Vitest never
// runs `.tsx`, so the arithmetic lives here and the component keeps only the pointer plumbing.

/** The height the map had before it could be resized (ADR-191 decision 9), and still the default. */
export const DEFAULT_GROUP_MAP_PX = 300;

/** Smallest usable map. Below this a level of more than a few boxes cannot be read at all. */
export const MIN_GROUP_MAP_PX = 200;

/** Largest fraction of the window the map may take, so the pane's header stays on screen. */
const MAX_VIEWPORT_FRACTION = 0.9;

/** Absolute ceiling, for a very tall window. */
const MAX_GROUP_MAP_PX = 1200;

/** Keyboard resize step. */
export const GROUP_MAP_STEP_PX = 40;

/** The tallest the map may be in this window, never below the floor. */
export function groupMapCeiling(viewportPx: number): number {
  // A zero or garbage viewport (an unmeasured window) must not collapse the ceiling to 0.
  const ceiling =
    viewportPx > 0
      ? Math.min(MAX_GROUP_MAP_PX, Math.round(viewportPx * MAX_VIEWPORT_FRACTION))
      : MAX_GROUP_MAP_PX;
  return Math.max(MIN_GROUP_MAP_PX, ceiling);
}

/** Hold a height inside the usable range for this window. */
export function clampGroupMapHeight(px: number, viewportPx: number): number {
  const ceiling = groupMapCeiling(viewportPx);
  // The floor is the OUTER bound (ADR-074), even though the ceiling already respects it: the order is
  // the part every handle in this product has got wrong once.
  return Math.max(MIN_GROUP_MAP_PX, Math.min(ceiling, Math.round(px)));
}

/** The height to draw: the operator's stored one, re-clamped for this window, or the default. */
export function groupMapHeight(stored: number | null, viewportPx: number): number {
  return clampGroupMapHeight(stored ?? DEFAULT_GROUP_MAP_PX, viewportPx);
}

/**
 * The height a drag produces: where the map started, plus how far the pointer has moved. Computed
 * from the gesture's origin rather than accumulated per move, which drifts when a move is dropped.
 * The handle sits under the map, so pulling it down makes the map taller.
 */
export function groupMapHeightFromDrag(
  startHeight: number,
  startClientY: number,
  clientY: number,
  viewportPx: number,
): number {
  return clampGroupMapHeight(startHeight + (clientY - startClientY), viewportPx);
}

/** One keyboard step. ArrowDown grows the map (the handle moves down), ArrowUp shrinks it; any
 *  other key is not this handle's, and answers null. */
export function groupMapHeightFromKey(current: number, key: string, viewportPx: number): number | null {
  const dir = key === 'ArrowDown' ? 1 : key === 'ArrowUp' ? -1 : 0;
  if (dir === 0) return null;
  return clampGroupMapHeight(current + dir * GROUP_MAP_STEP_PX, viewportPx);
}
