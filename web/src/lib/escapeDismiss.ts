// SPDX-License-Identifier: AGPL-3.0-only
// Whether an Escape key press may clear a *persistent* selection (ADR-073 decisions 3 and 4).
//
// Escape has layers in this product. Everything that existed before this file closes something the
// operator opened a moment ago — a modal, a popover, a context menu — and thirteen components each
// register their own `keydown` for it. What is new here is Escape reaching past those to something
// that survives a reload: the Nodes split's `?sel=`, the dashboard's edit mode. So the rule is
// ordering, not novelty — **the innermost open surface wins**, and the page's own selection is only
// touched when nothing is open above it.
//
// **Why a `.ts`.** The callers are all `.tsx`, and a `.tsx` test is a file nothing runs
// (`environment: 'node'` + `include: ['src/**/*.test.ts']`, see testing.md). Writing the same
// four-clause condition in three components is also how the third copy ends up wrong
// (`extensibility.md` §3), so the judgement lives here once and the components only supply facts.

/** Surfaces that float above a page: dialogs, popovers, the two remaining legacy menus. Each is in
 *  the DOM only while open, so "does one exist?" is the same question as "is one open?".
 *
 *  ⚠️ **Hand-maintained, and nothing checks it.** A fourth popover primitive that never gets added
 *  here makes one Escape both close the panel and clear the selection behind it. That failure is
 *  mild by construction — the selection is re-selectable in one click — which is why this is a
 *  documented limit rather than a blocker. What holds it together is `ui-conventions.md`'s existing
 *  rule that new popovers use `AnchoredPopover` (`.apop`) instead of becoming a fourth entry here;
 *  the legacy classes are a closed set that predates it, and it is shrinking: `.ovm-menu` left when
 *  `OverflowMenu` moved onto `ActionMenu` (ADR-088 Inc.3), so its entry was removed with it — a
 *  selector kept here for a surface that no longer exists is a line nobody can evaluate. */
const FLOATING_LAYERS = [
  '[role="dialog"]',
  '.apop',
  '.ntree-menu',
  '.ts-run-menu',
] as const;

/** Surfaces that live *inside* the page but still answer Escape before the page's selection does.
 *  Today that is the Interfaces dock, and the case is real rather than theoretical: on the Nodes
 *  split the dock renders inside the detail pane of the very selection Escape would otherwise
 *  clear, so one press would close the dock and throw away the node it belonged to. */
const IN_PAGE_LAYERS = ['.nd-if-dock'] as const;

/** Everything that outranks a **dialog**: the floating layers minus dialogs themselves, so a
 *  modal asking this is not blocked by its own presence in the document.
 *
 *  Derived from the same array rather than spelled out, for the reason the file exists. */
export const ABOVE_DIALOG_SELECTOR = FLOATING_LAYERS.filter(
  (s) => s !== '[role="dialog"]',
).join(', ');

/** Everything that outranks a page-level selection. */
export const OVERLAY_SELECTOR = [...FLOATING_LAYERS, ...IN_PAGE_LAYERS].join(', ');
/** Everything that outranks an in-page surface — the floating layers only, so a surface asking this
 *  is not blocked by itself. Derived from the same array, so the two cannot drift apart. */
export const FLOATING_SELECTOR = FLOATING_LAYERS.join(', ');

/** The facts a caller reads off the DOM. Passed in rather than read here so this stays DOM-free and
 *  therefore testable in the `node` environment Vitest runs. */
export interface EscapeContext {
  /** `KeyboardEvent.key`. */
  key: string;
  /** Whether any element outranking the caller is currently in the document. */
  overlayOpen: boolean;
  /** `document.activeElement`'s tag name, as the DOM reports it (upper-case). */
  tagName?: string;
  /** `document.activeElement`'s `isContentEditable`. */
  isEditable: boolean;
}

/** Whether this key press should dismiss what the caller owns.
 *
 *  False for every key but Escape; false while something above the caller is open (it gets the
 *  press); and false while the operator is typing, because Escape belongs to the field then — the
 *  same reasoning as `searchBox.ts::shouldFocusOnSlash`, which refuses to steal `/` out of an input. */
export function shouldDismissOnEscape(ctx: EscapeContext): boolean {
  if (ctx.key !== 'Escape') return false;
  if (ctx.overlayOpen) return false;
  if (ctx.isEditable) return false;
  switch ((ctx.tagName ?? '').toUpperCase()) {
    case 'INPUT':
    case 'TEXTAREA':
    case 'SELECT':
      return false;
    default:
      return true;
  }
}

function ask(e: KeyboardEvent, selector: string): boolean {
  const active = document.activeElement as HTMLElement | null;
  return shouldDismissOnEscape({
    key: e.key,
    overlayOpen: document.querySelector(selector) !== null,
    tagName: active?.tagName,
    isEditable: active?.isContentEditable ?? false,
  });
}

/** For a selection that outlives the press — the Nodes split's `?sel=`, the dashboard's edit mode.
 *  Yields to floating layers AND to in-page surfaces. */
export function escapeClearsSelection(e: KeyboardEvent): boolean {
  return ask(e, OVERLAY_SELECTOR);
}

/** For a surface listed in {@link IN_PAGE_LAYERS} closing itself. Yields only to floating layers,
 *  so the caller is not blocked by its own presence in the document. */
export function escapeClosesInPageSurface(e: KeyboardEvent): boolean {
  return ask(e, FLOATING_SELECTOR);
}

/** For a modal dialog closing itself. Yields to a popover or menu opened **from inside it**, and to
 *  nothing else.
 *
 *  It deliberately does **not** go through {@link shouldDismissOnEscape}: that refuses while the
 *  operator is typing, which is right for a page-level selection and wrong for a dialog — a form is
 *  mostly text inputs, and Escape has always closed it from one. The only question here is whether
 *  something is stacked above.
 *
 *  ⚠️ The case is real, not theoretical. `AnchoredPopover` and `Modal` both listen on `document`,
 *  so before this the metric picker's Escape closed the popover **and** threw away the half-filled
 *  rule behind it. Nothing had noticed because the metric picker is the first popover this product
 *  has ever put inside a dialog. */
export function escapeClosesDialog(e: KeyboardEvent): boolean {
  return e.key === 'Escape' && document.querySelector(ABOVE_DIALOG_SELECTOR) === null;
}
