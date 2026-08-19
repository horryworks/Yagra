// SPDX-License-Identifier: AGPL-3.0-only
// How wide the All-nodes inventory pane is, and what a drag of the split handle does to it.
//
// A `.ts` module because the clamping is the part that can be silently wrong, and Vitest never runs
// `.tsx` — the arithmetic lives here and the page keeps only the pointer plumbing. Same split as
// `mapPaneHeight.ts` and `components/NodeDetail/interfaceDockHeight.ts`.
//
// THIS IS THE THIRD RESIZE HANDLE, AND IT IS THE FIRST HORIZONTAL ONE. What that changes:
//
//  1. **The axis and the sign.** The handle sits to the RIGHT of the tree, so dragging right
//     (a larger clientX) grows it. ArrowRight grows; the two vertical handles use ArrowUp/Down and
//     disagree with each other about which one grows.
//  2. **The ceiling subtracts the handle itself.** The handle occupies a real 14px grid column
//     between the two panes — it *is* the gap — so the width available to the two panes is
//     `container - HANDLE_PX`, not `container`. Neither vertical handle has a middle element.
//
// What is deliberately the same is the trap both of the others document: the floor must be the
// OUTER bound of the clamp. `clampTreeWidth`'s test says so.

/**
 * Smallest usable inventory pane.
 *
 * Derived, not chosen: the pane head carries the INVENTORY label, the « collapse button, the ＋
 * menu and a 150px search box. Below ~220px the search box is the first thing to be squeezed out
 * of the row, and a tree you cannot search is not a narrower tree — it is a broken one.
 *
 * ⚠️ Read off `NodesPage.css`, not measured in a browser. If the pane head changes, this is the
 * number that silently stops meaning what its name says.
 */
export const TREE_MIN_PX = 220;

/**
 * Space the detail pane always keeps.
 *
 * The node detail is the widest thing on the page: the Interfaces table alone carries eight
 * columns since ADR-063. 420px is not comfortable, but it is the point below which the tab bar
 * starts wrapping — the operator asked for a wider tree, so let them have it right up to there.
 *
 * There is no equivalent floor for "the detail is unusable, give up": the pane is never hidden by
 * this handle, and the « rail is the control for wanting it full-width.
 */
export const DETAIL_MIN_PX = 420;

/**
 * The handle's own grid column, in px. **A second copy of a number in `NodesPage.css`**, and it is
 * the same 14px the grid `gap` used to be — the handle replaced the gap rather than being added
 * beside it, so the panes sit exactly where they always did.
 */
export const HANDLE_PX = 14;

/** Keyboard resize step, so the handle is operable without a pointer (ui-conventions.md). */
export const PANE_STEP_PX = 24;

/**
 * Width of the pane for someone who has never dragged the handle.
 *
 * The literal `312px` this page shipped with since it was written. Unlike the Interfaces dock —
 * where the default was deliberately made *larger* than the old one — nothing here is being fixed
 * by the default, so it stays byte-identical for anyone who never touches the handle.
 */
export const DEFAULT_TREE_PX = 312;

/**
 * Hold a width inside the usable range for a split of `containerPx`.
 *
 * ⚠️ The floor is the OUTER bound on purpose. On a container too narrow to satisfy both floors the
 * ceiling falls below `TREE_MIN_PX`, and applying it last would shrink the tree to nothing — the
 * ordering trap `mapPaneHeight.ts` learned first. A squeezed detail pane is recoverable (drag back,
 * or press «); a tree you cannot select a node in is not.
 */
export function clampTreeWidth(px: number, containerPx: number): number {
  // A zero/garbage container (first render, before the split is measured) must not collapse the
  // floor to 0 — fall back to the floor alone rather than inventing a ceiling.
  const ceiling =
    containerPx > 0 ? containerPx - HANDLE_PX - DETAIL_MIN_PX : Number.POSITIVE_INFINITY;
  return Math.max(TREE_MIN_PX, Math.min(ceiling, Math.round(px)));
}

/** The largest width this container allows, for `aria-valuemax`. */
export function maxTreeWidth(containerPx: number): number {
  return clampTreeWidth(Number.POSITIVE_INFINITY, containerPx);
}

/**
 * The stored preference resolved for this container. `null` means "never resized".
 *
 * Re-clamping a stored value matters even though the preference is browser-local: the same browser
 * gets narrower when the window is resized or the nav sidebar is expanded, and a width dragged out
 * on a maximised window would otherwise leave no detail pane at all on a half-screen one.
 */
export function resolveTreeWidth(stored: number | null, containerPx: number): number {
  return clampTreeWidth(stored ?? DEFAULT_TREE_PX, containerPx);
}

/**
 * The width a drag produces: where the pane started, plus how far the pointer has moved right.
 *
 * Computed from the gesture's **origin** rather than accumulated per move event, so a coalesced or
 * dropped pointermove cannot make the pane creep away from the cursor over a long drag.
 */
export function widthFromDrag(
  startWidth: number,
  startClientX: number,
  clientX: number,
  containerPx: number,
): number {
  return clampTreeWidth(startWidth + (clientX - startClientX), containerPx);
}

/**
 * Apply one keyboard step, or `null` when `key` is not one this handle claims.
 *
 * Takes the key name rather than a direction so the axis is decided *here*, where a test can reach
 * it: this handle is horizontal, and the two that came before it are not.
 */
export function widthFromKey(
  current: number,
  key: string,
  containerPx: number,
): number | null {
  const dir = key === 'ArrowRight' ? 1 : key === 'ArrowLeft' ? -1 : 0;
  if (dir === 0) return null;
  return clampTreeWidth(current + dir * PANE_STEP_PX, containerPx);
}
