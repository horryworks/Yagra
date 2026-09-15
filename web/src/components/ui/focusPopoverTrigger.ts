// SPDX-License-Identifier: AGPL-3.0-only
// Kept apart from `AnchoredPopover.tsx` so that file exports only a component (react-refresh's rule).

import type { PopoverRole } from './AnchoredPopover';

/** Move focus back to the popover's trigger. Callers that close on Escape use this; the selector is
 *  the same one `AnchoredPopover` measures from, so the two cannot disagree.
 *
 *  🚨 **`preventScroll`, for the same reason every other focus in this directory passes it** — and
 *  this was the one call site that did not, for its whole life, while six others got it right. A
 *  trigger can be half-clipped inside a scroller (a virtualized tree row is the case that found
 *  this), and the browser then scrolls its ancestor to show it: closing a menu moved the pane
 *  underneath it. Focus is still moved; only the scroll is refused (ADR-124 増分 5 決定 D). */
export function focusPopoverTrigger(
  anchor: HTMLElement | null | undefined,
  role: PopoverRole,
): void {
  anchor?.querySelector<HTMLElement>(`[aria-haspopup="${role}"]`)?.focus({ preventScroll: true });
}
