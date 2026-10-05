// SPDX-License-Identifier: AGPL-3.0-only
// How big a resizable dialog is, and what a drag or a key on one of its handles does to that
// (ADR-198). The sixth resize handle, the same shape as the other five (ADR-074): the clamp and the
// drag arithmetic live here because Vitest never runs `.tsx`, and the component keeps only the
// pointer plumbing.
//
// ⚠️ **The first handle that grows a CENTRED box.** `.modal-overlay` centres the dialog on both axes,
// so moving one edge by `d` changes the size by `2d`: the dialog grows on both sides, and its edge
// stays under the pointer. Every earlier handle sat on a box anchored at one side, where `d` was
// the whole change.

/**
 * The dialogs an operator may resize (ADR-198 decision 1) — the ones holding code, a certificate,
 * JSON, a log or a preview, whose reading depends on where the lines break. Each name is used by
 * exactly one `<Modal resizeId=…>`, which `modalSize.test.ts` checks against the source.
 */
export const MODAL_RESIZE_IDS = [
  'apiToken',
  'busHandoff',
  'channelTemplate',
  'forwardingDest',
  'netboxIntegration',
  'pollerRegister',
  'rca',
  'reportViewer',
] as const;
export type ModalResizeId = (typeof MODAL_RESIZE_IDS)[number];

/** What the operator dragged one dialog to. `null` on an axis = never resized there, so the
 *  dialog keeps the CSS size its `size` prop gives it. */
export interface ModalSize {
  w: number | null;
  h: number | null;
}

/** Narrowest usable dialog. Below this a form's labels and a code line both stop fitting. */
export const MIN_MODAL_W = 400;
/** Shortest usable dialog: the header, the footer and a few lines of body. */
export const MIN_MODAL_H = 240;
/** Keyboard resize step. */
export const MODAL_STEP_PX = 40;

/** The margins `Modal.css` keeps around a dialog: 48px across, 96px down. */
const MARGIN_W = 48;
const MARGIN_H = 96;

/** The widest a dialog may be in this window, never below the floor. A zero (unmeasured) window
 *  must not collapse the ceiling to the floor-or-less, so it falls back to the floor itself. */
export function modalWidthCeiling(viewportW: number): number {
  return Math.max(MIN_MODAL_W, viewportW > 0 ? viewportW - MARGIN_W : MIN_MODAL_W);
}

/** The tallest a dialog may be in this window, never below the floor. */
export function modalHeightCeiling(viewportH: number): number {
  return Math.max(MIN_MODAL_H, viewportH > 0 ? viewportH - MARGIN_H : MIN_MODAL_H);
}

/** Hold a width inside the usable range for this window. The floor is the OUTER bound (ADR-074). */
export function clampModalWidth(px: number, viewportW: number): number {
  return Math.max(MIN_MODAL_W, Math.min(modalWidthCeiling(viewportW), Math.round(px)));
}

/** Hold a height inside the usable range for this window. The floor is the OUTER bound. */
export function clampModalHeight(px: number, viewportH: number): number {
  return Math.max(MIN_MODAL_H, Math.min(modalHeightCeiling(viewportH), Math.round(px)));
}

/** The size to draw: each stored axis re-clamped for this window, and `null` where the operator
 *  never resized (the CSS default then applies). Re-clamping on every read is what keeps a size
 *  dragged on a big monitor from overflowing a laptop. */
export function resolveModalSize(stored: ModalSize | undefined, viewportW: number, viewportH: number): ModalSize {
  return {
    w: stored?.w == null ? null : clampModalWidth(stored.w, viewportW),
    h: stored?.h == null ? null : clampModalHeight(stored.h, viewportH),
  };
}

/** Which axes a handle moves. */
export type ModalEdge = 'right' | 'bottom' | 'corner';

/**
 * The size a drag produces, computed from the gesture's origin rather than accumulated per move.
 * `start` is the dialog's measured size when the press began; the centred dialog grows by twice
 * the pointer's travel. An axis the edge does not move keeps its start value.
 */
export function modalSizeFromDrag(
  edge: ModalEdge,
  start: { w: number; h: number },
  startClient: { x: number; y: number },
  client: { x: number; y: number },
  viewport: { w: number; h: number },
): { w: number; h: number } {
  const movesW = edge !== 'bottom';
  const movesH = edge !== 'right';
  return {
    w: movesW ? clampModalWidth(start.w + 2 * (client.x - startClient.x), viewport.w) : start.w,
    h: movesH ? clampModalHeight(start.h + 2 * (client.y - startClient.y), viewport.h) : start.h,
  };
}

/** One keyboard step on the right edge: ArrowRight widens (the edge moves right), ArrowLeft
 *  narrows. Any other key is not this handle's, and answers null. */
export function modalWidthFromKey(current: number, key: string, viewportW: number): number | null {
  const dir = key === 'ArrowRight' ? 1 : key === 'ArrowLeft' ? -1 : 0;
  if (dir === 0) return null;
  return clampModalWidth(current + dir * MODAL_STEP_PX, viewportW);
}

/** One keyboard step on the bottom edge: ArrowDown makes the dialog taller (the edge moves down),
 *  ArrowUp shorter. Any other key answers null. */
export function modalHeightFromKey(current: number, key: string, viewportH: number): number | null {
  const dir = key === 'ArrowDown' ? 1 : key === 'ArrowUp' ? -1 : 0;
  if (dir === 0) return null;
  return clampModalHeight(current + dir * MODAL_STEP_PX, viewportH);
}

/** The stored map after one dialog's size changes. Resetting both axes drops the entry, so the
 *  map holds only dialogs somebody actually resized. */
export function withModalSize(
  all: Readonly<Partial<Record<ModalResizeId, ModalSize>>>,
  id: ModalResizeId,
  size: ModalSize,
): Partial<Record<ModalResizeId, ModalSize>> {
  const next = { ...all };
  if (size.w == null && size.h == null) delete next[id];
  else next[id] = size;
  return next;
}
