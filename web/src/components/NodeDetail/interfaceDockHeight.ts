// SPDX-License-Identifier: AGPL-3.0-only
// How tall the Interfaces detail dock is, and what a drag of its top edge does to it (issue #65).
//
// A `.ts` module because the clamping is the part that can be silently wrong, and Vitest never runs
// `.tsx` — the arithmetic lives here and the component keeps only the pointer plumbing.
//
// TWO THINGS DIFFER FROM `pages/mapPaneHeight.ts`, AND BOTH ARE EASY TO COPY WRONG:
//
//  1. **The sign.** The Geo map's handle sits BELOW its pane, so dragging down grows it. This one
//     sits ABOVE the dock, so dragging *up* (a smaller clientY) grows it.
//  2. **The ceiling is a subtraction, not a fraction.** The map keeps 92% of the *window*; this
//     keeps whatever is left after the interface list above it gets its floor. They answer different
//     questions — "leave the page header and legend on screen" versus "leave three interface rows
//     visible" — which is why the two modules are separate rather than one parameterized helper.
//     Changing the map's ceiling should NOT change this one.
//
// What is deliberately the same is the trap both have to avoid, and `mapPaneHeight.ts` learned it
// first: the floor must be the OUTER bound of the clamp. `clampDockHeight`'s test says so.

/**
 * Smallest usable dock.
 *
 * Derived, not chosen: the dock's own chrome (the handle, the head row, the chart title, uPlot's
 * legend, the padding and two gaps) costs roughly 120px, and the floor exists to guarantee **the
 * charts never get smaller than the 132px they were before this shipped**. 120 + 132 ≈ 260.
 *
 * ✅ Measured 2026-09-01 (ADR-058 ①), in a real layout engine at 1280x1000: dock 317px, chart
 * plot 204px, so the chrome costs **113px** — the 120 above was 7px conservative, which is the
 * safe direction, and the floor therefore leaves 147px of chart against a bar of 132.
 *
 * ⚠️ The number that matters is not 113, it is the budget: this constant only means what its name
 * says while the chrome fits in `DOCK_MIN_PX - 132` = 128px. `tests/ui/interfaceDock.spec.ts`
 * asserts exactly that, in a browser, so a chrome that grows fails a test instead of silently
 * shipping charts smaller than the ones this replaced.
 */
export const DOCK_MIN_PX = 260;

/**
 * Space the interface list above the dock always keeps: `.nd-if-head` (32px) plus three
 * `.nd-if-row` (42px each).
 *
 * The sticky filter row is deliberately not counted. It is closed by default since ADR-053 Inc.9,
 * and reserving its 34px would leave a 720p window almost no drag travel at all.
 */
export const LIST_MIN_PX = 160;

/** Keyboard resize step, so the handle is operable without a pointer (ui-conventions.md). */
export const DOCK_STEP_PX = 40;

/**
 * Fraction of the container the dock takes when the operator has never resized it.
 *
 * This is the value `.nd-if-dock`'s `max-height` used to cap it at. Turning the old *ceiling* into
 * the new *default* is what answers issue #65 for someone who never finds the drag handle: the dock
 * used to size to its content (~210px, charts at 132px) and now opens at ~390px on a 1080p screen.
 */
const DEFAULT_FRACTION = 0.46;

/**
 * Hold a height inside the usable range for a container of `containerPx`.
 *
 * ⚠️ The floor is the OUTER bound on purpose. On a container too short to satisfy both floors the
 * ceiling falls below `DOCK_MIN_PX`, and applying it last would shrink the dock to nothing — the
 * ordering trap `mapPaneHeight.ts` documents. Losing the list is recoverable (the dock's × closes
 * it); losing the dock is not.
 */
export function clampDockHeight(px: number, containerPx: number): number {
  // A zero/garbage container (first render, before the pane is measured) must not collapse the
  // floor to 0 — fall back to the floor alone rather than inventing a ceiling.
  const ceiling = containerPx > 0 ? containerPx - LIST_MIN_PX : Number.POSITIVE_INFINITY;
  return Math.max(DOCK_MIN_PX, Math.min(ceiling, Math.round(px)));
}

/** The height an operator who has never dragged the handle gets, for this container. */
export function defaultDockHeight(containerPx: number): number {
  return clampDockHeight(Math.round(containerPx * DEFAULT_FRACTION), containerPx);
}

/**
 * The stored preference resolved for this container. `null` means "never resized".
 *
 * Re-clamping a stored value matters because the preference now follows the account across machines
 * (ADR-058): a height dragged out on a 1440p monitor would otherwise swallow the whole list when the
 * same person signs in on a laptop.
 */
export function resolveDockHeight(stored: number | null, containerPx: number): number {
  return stored == null ? defaultDockHeight(containerPx) : clampDockHeight(stored, containerPx);
}

/**
 * The height a drag produces: where the dock started, minus how far the pointer has moved down.
 *
 * ⚠️ The subtraction is the inversion — the handle is *above* the dock, so moving the pointer up
 * (`clientY` decreasing) makes the dock taller. `mapPaneHeight.heightFromDrag` adds.
 *
 * Computed from the gesture's **origin** rather than accumulated per move event, so a coalesced or
 * dropped pointermove cannot make the dock creep away from the cursor over a long drag.
 */
export function heightFromDrag(
  startHeight: number,
  startClientY: number,
  clientY: number,
  containerPx: number,
): number {
  return clampDockHeight(startHeight - (clientY - startClientY), containerPx);
}

/**
 * Apply one keyboard step, or `null` when `key` is not one this handle claims.
 *
 * Takes the key name rather than a direction so the inversion is decided *here*, where a test can
 * reach it: ArrowUp grows, which is the opposite of the Geo map's handle.
 */
export function heightFromKey(
  current: number,
  key: string,
  containerPx: number,
): number | null {
  const dir = key === 'ArrowUp' ? 1 : key === 'ArrowDown' ? -1 : 0;
  if (dir === 0) return null;
  return clampDockHeight(current + dir * DOCK_STEP_PX, containerPx);
}

// ── Measuring the chrome the dock and the list share ────────────────────────────────────────────
//
// Both of these read the DOM, and both are here rather than in the component for the reason the
// arithmetic above is: a mirror of a CSS value is a number that goes stale silently. They take an
// element and ask it, so a test can hand them a stand-in and pin what they add up.

/** The list's sticky chrome — the header and, on a desktop, the filter row under it.
 *
 *  ⚠️ **Measured, never mirrored.** It was a `LIST_HEAD_H = 32` constant, and both of the things a
 *  mirror does went wrong: ADR-053 added a 34px sticky filter row below the header and nobody
 *  updated the number, so a selected row near the top scrolled *under* the filter row; and mobile
 *  hides both, so the number reserved 32px of nothing. A `display: none` element measures 0, which
 *  is exactly the answer wanted in that case. */
export function stickyChromeHeight(list: HTMLElement): number {
  return Array.from(list.querySelectorAll('.nd-if-head, .nd-if-filters')).reduce(
    (h, el) => h + el.getBoundingClientRect().height,
    0,
  );
}

/** The height `.nd-if-list` and the dock share: the tab body minus the fixed chrome above them —
 *  the toolbar, plus the error banner when one is showing.
 *
 *  ⚠️ NOT `window.innerHeight`. This pane sits under the app shell's top bar, the node identity
 *  header and the tab strip, which cost roughly 230px on a 720p window — sizing the dock against
 *  the window would let it push the list off the bottom.
 *  ⚠️ A future `flex: none` sibling of `.nd-if` must be added to this selector, or the dock
 *  over-claims by that sibling's height. */
export function dockBudget(root: HTMLElement): number {
  const chrome = Array.from(
    root.querySelectorAll(':scope > .nd-if-toolbar, :scope > .form-error'),
  ).reduce((h, el) => h + el.getBoundingClientRect().height, 0);
  return root.clientHeight - chrome;
}
