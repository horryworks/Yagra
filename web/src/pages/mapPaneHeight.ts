// SPDX-License-Identifier: AGPL-3.0-only
// How tall the Geo map's pane is, and what a drag of the resize handle does to it.
//
// A `.ts` module because the clamping is the part that can be silently wrong — a handle that lets
// the pane grow past the window turns the page into a scroll trap, and one that lets it collapse to
// nothing loses the map with no way to get it back. Vitest never runs `.tsx`, so the arithmetic
// lives here and the component keeps only the pointer plumbing.

/** Smallest usable pane. Below this the world outline is too small to locate anything on. */
export const MIN_PANE_PX = 260;

/** Largest fraction of the window the pane may take, so the header and legend stay on screen. */
const MAX_VIEWPORT_FRACTION = 0.92;

/** Absolute ceiling, for a very tall window where 92% would still be more map than anyone wants. */
const MAX_PANE_PX = 1600;

/**
 * The default height for a window of `viewportPx`.
 *
 * ~62% of the window, which puts the pane at roughly a 2:1 box on a normal laptop — close to the
 * map's own 2:1 aspect, so a fitted world fills it instead of sitting in a letterbox. The first cut
 * used `flex: 1`, which collapsed to a ~150px strip because the app shell's content area is not a
 * height-constrained flex column; an explicit height is what the topology map does, for the same
 * reason.
 */
export function defaultPaneHeight(viewportPx: number): number {
  return clampPaneHeight(Math.round(viewportPx * 0.62), viewportPx);
}

/** Hold a height inside the usable range for this window. */
export function clampPaneHeight(px: number, viewportPx: number): number {
  // A zero/garbage viewport (server render, an unmeasured pane) must not collapse the floor to 0.
  const ceiling =
    viewportPx > 0 ? Math.min(MAX_PANE_PX, Math.round(viewportPx * MAX_VIEWPORT_FRACTION)) : MAX_PANE_PX;
  // Guard the inversion: on a very short window the fraction can fall below the floor, and
  // `Math.max(MIN, Math.min(MIN, …))` order matters — the floor must win, or the pane vanishes.
  return Math.max(MIN_PANE_PX, Math.min(ceiling, Math.round(px)));
}

/**
 * The height a drag produces: where the pane started, plus how far the pointer has moved.
 *
 * Deliberately computed from the gesture's **origin** rather than accumulated per move event. An
 * accumulating version drifts whenever a move is coalesced or dropped, so the pane creeps away from
 * the cursor over a long drag.
 */
export function heightFromDrag(
  startHeight: number,
  startClientY: number,
  clientY: number,
  viewportPx: number,
): number {
  return clampPaneHeight(startHeight + (clientY - startClientY), viewportPx);
}

/** Keyboard resize step, so the handle is operable without a pointer (ui-conventions.md). */
export const PANE_STEP_PX = 40;

/** Apply one keyboard step. `dir` is -1 for shrink (ArrowUp), +1 for grow (ArrowDown). */
export function heightFromKey(current: number, dir: -1 | 1, viewportPx: number): number {
  return clampPaneHeight(current + dir * PANE_STEP_PX, viewportPx);
}
