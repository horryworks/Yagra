// SPDX-License-Identifier: AGPL-3.0-only
// Why the inventory tree does not move unless the operator scrolls it (ADR-124 増分 5).
//
// The tree never scrolled itself: `scrollTo` / `scrollIntoView` / `scrollTop` are written nowhere in
// `NodeTree.tsx` or `NodesPage.tsx`, and the virtualizer's own scroll-writing paths are unreachable
// at that call site (no `measureElement`, `anchorTo` left at the default `start`). What moved the
// pane was the **browser**: `.ntree-node-name` is a real `<button>` at `flex: 1`, so it covers
// almost the whole row, and the browser scrolls a newly-focused element into view when it is not
// fully visible. On a 30px row grid the first and last rows are almost always half-clipped, so
// nearly every click nudged the scroller by up to 29px.
//
// 🚨 **The fix cannot be `preventDefault()` on mousedown.** That suppresses the mouse-focus, and it
// also stops Chrome starting the row's native drag — which carries the whole working set since
// 増分 4. The name button covers the row, so it is also where a drag naturally begins. So the
// browser keeps its focus; what it does not get is the scroll.
//
// 🚨 **One pre-empt at the container, not one per control.** A row holds `.ntree-twisty`,
// `.ntree-grp-name`, `.ntree-node-name`, `.ntree-act` ×4 and `.ntree-supp-icon` ×4 — eight sites
// today and one more per control added later, each a silent regression with nothing to fail
// (`extensibility.md` §1). Asking the DOM which control a press resolves to covers every one of
// them, and every control a future increment adds, from a single call site.
//
// ⚠️ **No `HTMLElement` in any signature.** Every parameter is the smallest structural type that
// does the job, so a test hands over a plain object — `.tsx` files are never executed by the runner
// (`.claude/rules/testing.md`), which is why the judgement lives here and not beside the handler.

/** The two numbers a scroll container carries. `HTMLDivElement` satisfies this structurally. */
export interface ScrollBox {
  scrollTop: number;
  scrollLeft: number;
}

/** Where a scroller was parked when a gesture started. */
export interface ScrollAt {
  readonly top: number;
  readonly left: number;
}

/** A control that can be focused without the browser scrolling to show it. */
export interface Focusable {
  focus(options: { preventScroll: boolean }): void;
}

/** Whatever a press landed on: an element able to name its nearest focusable ancestor. */
export interface PressTarget {
  closest(selector: string): Focusable | null;
}

/**
 * What may take focus inside a tree row.
 *
 * Every control in a row is a `<button>` today, and no row is focusable itself — there is no
 * `tabIndex` anywhere in `NodeTree.tsx`. The other forms are listed so a future link or text entry
 * is covered without a second edit, which is the point of asking the DOM rather than naming the
 * eight class names that exist right now.
 */
export const FOCUSABLE_IN_ROW = 'button, a[href], input, select, textarea, [tabindex]';

/** The control a press will focus, or null when it landed on blank space (or the scrollbar). */
export function focusTargetOf(target: PressTarget | null): Focusable | null {
  return target?.closest(FOCUSABLE_IN_ROW) ?? null;
}

/** Remember where a scroller is parked. Null before the ref is attached. */
export function scrollAt(box: ScrollBox | null): ScrollAt | null {
  return box ? { top: box.scrollTop, left: box.scrollLeft } : null;
}

/**
 * The mousedown step: focus the control this press was going to focus anyway, **without** the
 * scroll the browser's own focusing step performs, and record where the scroller was in case
 * something scrolls it regardless.
 *
 * 🚨 **The pin is recorded BEFORE the focus, and that order is the whole reason this takes a
 * callback instead of returning the value.** `focus()` dispatches `focusin` **synchronously**, so
 * a caller that wrote `pinned.current = pinFocusScroll(…)` had the restore handler run while the
 * ref was still null — the backstop was disarmed by the pre-empt in front of it, and every test
 * that only measured the end state passed. Measured: the tree still moved 2px on a clipped row.
 *
 * 🚨 **`pin(null)` for a press on blank space, and that branch is load-bearing too.** A pin nothing
 * consumes goes stale: press the scrollbar (which fires no `click` on the body, so nothing resets
 * it), wheel somewhere else, then `Tab` into a row — and a pin taken from any press would drag the
 * operator back to where they were before their own wheel. Pinning only when a control was
 * actually pressed is what makes "the position changes when a human moves it" true both ways.
 *
 * ⚠️ Firefox and Safari do not focus a `<button>` on mousedown at all, so there the pre-empt *adds*
 * a focus the browser would not have given. Harmless — the row controls have no `:focus` rule and
 * the UA ring is `:focus-visible`, which a pointer press does not set — but it is a real
 * difference, and no suite in this repo runs those browsers.
 */
export function pinFocusScroll(
  box: ScrollBox | null,
  target: PressTarget | null,
  pin: (at: ScrollAt | null) => void,
): void {
  const control = focusTargetOf(target);
  if (!control) {
    pin(null);
    return;
  }
  pin(scrollAt(box));
  control.focus({ preventScroll: true });
}

/**
 * Put a pinned offset back — the mechanism-independent half, behind the pre-empt.
 *
 * Returns **whether it had to write anything**, for two reasons: an unchanged offset written back is
 * a needless layout write on every click, and a caller that always wrote would be
 * indistinguishable, to a test, from one that never did.
 *
 * A null pin means there was no press to restore from — a keyboard `Tab` into a row, where the
 * browser scrolling the target into view is the correct behaviour and must not be undone.
 */
export function restoreScroll(box: ScrollBox | null, at: ScrollAt | null): boolean {
  if (!box || !at) return false;
  if (box.scrollTop === at.top && box.scrollLeft === at.left) return false;
  box.scrollTop = at.top;
  // Both axes: `.ntree-body` declares only `overflow-y`, but CSS computes the other axis to `auto`
  // when one of a pair is `visible`, so a deep row with a long name scrolls sideways too.
  box.scrollLeft = at.left;
  return true;
}
