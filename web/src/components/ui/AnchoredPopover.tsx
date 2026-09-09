// SPDX-License-Identifier: AGPL-3.0-only
// A popover measured from a trigger — or opened at a point — rendered through a portal, and
// dismissed the way the rest of the app dismisses things. Extracted from `ActionMenu`, which is now
// its first caller.
//
// **Why this exists rather than a fourth copy.** `ActionMenu`'s own header asked for it: the popover
// chrome had already been re-typed three times (`.ovm-menu`, troubleshoot's `.ts-run-menu`,
// NodeTree's `.ntree-menu`) and `ActionMenu` was the fourth. The column filter row (ADR-053) needs
// two more shapes — a multi-select listbox and a text-condition dialog — so the choice was one
// primitive or six copies. It is extracted *and adopted in the same change*, because an abstraction
// with one consumer is just a rename: only migrating `ActionMenu` onto it proves the seam is in the
// right place.
//
// **Every part of "fixed + portal" is load-bearing, and the two callers need it for DIFFERENT
// reasons — which is exactly why it belongs here rather than in either of them.**
//   - `ActionMenu`'s callers sit in `.nodes-pane`, `overflow: hidden` inside a 312px grid column, so
//     an absolutely-positioned menu is clipped whichever edge it aligns to.
//   - A virtualized row carries `transform: translateY(...)`, which establishes a containing block
//     for `position: fixed` descendants — so a menu rendered in place inside a `DataTable` row lands
//     far outside the scroller, invisible, wide enough to raise a horizontal scrollbar. The only
//     visible symptom is that stray scrollbar.
//   - The filter row is not virtualized and has no transform, but `.dt` is `overflow: hidden`
//     (DataTable.css), so "simplifying" this to `position: absolute` clips the leftmost column's
//     popover at the table's bottom edge.
// The portal is what makes `fixed` mean the viewport again. Do not re-derive the positioning
// anywhere else.
//
// **Two anchors (ADR-124 Inc.2).** A popover that opens from a control passes `anchorRef` and is
// placed from the trigger's rect. A context menu passes `at` — the right-click's `clientX/Y` — and
// is placed from that point the way a native menu opens: down-and-right, flipping up or left when
// that does not fit, and scrolling inside a `maxHeight` when it is taller than the screen. The
// arithmetic for both is `popoverPlacement.ts`, where a unit test can reach it; the trigger half
// lived in this file's `useCallback` for four increments and was run by nothing but a browser.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { KeyboardEvent, ReactNode, RefObject } from 'react';
import {
  placeFromPoint,
  placeFromTrigger,
  samePlacement,
  type Placement,
  type Point,
} from './popoverPlacement';
import './AnchoredPopover.css';

/** Roles a popover may take. Each is also a valid `aria-haspopup` value, which is what lets the
 *  trigger be found by the attribute this component itself supplies — so the selector cannot drift,
 *  and no `forwardRef` is needed on Button/IconButton (neither has one). */
export type PopoverRole = 'menu' | 'listbox' | 'dialog';

interface Props {
  open: boolean;
  /** The element wrapping the trigger. Outside-clicks are measured against this **and** the popover:
   *  the popover is portalled, so it is not inside the anchor in the DOM, and asking only the anchor
   *  closes the popover before its own controls get their click. Required unless `at` is given. */
  anchorRef?: RefObject<HTMLElement | null>;
  /** A viewport point to open at instead of a trigger — a right-click's `clientX/Y`. With it the
   *  popover is a context menu: placed from the point (`placeFromPoint`), and closed by any
   *  mousedown it does not itself contain. `anchorRef` is not read for placement when this is set. */
  at?: Point;
  role: PopoverRole;
  /** Accessible name for the popover itself. */
  label: string;
  /** Called to close. `restoreFocus` is true for Escape (focus goes back to the trigger) and false
   *  for an outside click (focus belongs wherever the operator just clicked). */
  onDismiss: (restoreFocus: boolean) => void;
  /** Which trigger edge the popover aligns to before it is clamped to the viewport. */
  align?: 'start' | 'end';
  /** Extra classes on the popover surface, for per-caller padding and widths. */
  className?: string;
  /** Told when the popover has been measured. A caller that moves focus inside must wait for this:
   *  the popover is `visibility: hidden` until placed, and a hidden element cannot take focus. */
  onPlacedChange?: (placed: boolean) => void;
  onKeyDown?: (e: KeyboardEvent<HTMLDivElement>) => void;
  children: ReactNode;
}

export function AnchoredPopover({
  open,
  anchorRef,
  at,
  role,
  label,
  onDismiss,
  align = 'end',
  className,
  onPlacedChange,
  onKeyDown,
  children,
}: Props) {
  /** Measured position. `null` until the layout effect has run — the popover stays invisible until
   *  then, so it never paints at the wrong place first. */
  const [pos, setPos] = useState<Placement | null>(null);
  const popRef = useRef<HTMLDivElement>(null);

  const triggerEl = useCallback(
    () => anchorRef?.current?.querySelector<HTMLElement>(`[aria-haspopup="${role}"]`) ?? null,
    [anchorRef, role],
  );

  // Primitives, so a caller writing `at={{ x: menu.x, y: menu.y }}` inline does not re-place the
  // popover on every one of its own renders.
  const atX = at?.x;
  const atY = at?.y;

  /** Measure the popover and place it inside the viewport. */
  const place = useCallback(() => {
    const el = popRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    // 🚨 The NATURAL height, not the laid-out one. Once a `maxHeight` is applied the box is shorter
    // than its content, and measuring that would say "it fits now", drop the ceiling, let it grow,
    // and be back here to shrink it again — an oscillation the browser reports as a ResizeObserver
    // loop. `scrollHeight` is the content whatever the box was told to be; the border comes back
    // from the difference between the offset and client heights.
    const natural = el.scrollHeight + (el.offsetHeight - el.clientHeight);
    const m = { width: rect.width, height: Math.max(rect.height, natural) };
    const vp = { width: window.innerWidth, height: window.innerHeight };
    let next: Placement;
    if (atX !== undefined && atY !== undefined) {
      next = placeFromPoint({ x: atX, y: atY }, m, vp);
    } else {
      const t = triggerEl()?.getBoundingClientRect();
      if (!t) return;
      next = placeFromTrigger(t, m, vp, align);
    }
    setPos((prev) => (samePlacement(prev, next) ? prev : next));
  }, [align, triggerEl, atX, atY]);

  // Runs before paint, so the first frame is already in the right place.
  useLayoutEffect(() => {
    if (open) place();
    else setPos(null);
  }, [open, place]);

  const placedRef = useRef(onPlacedChange);
  placedRef.current = onPlacedChange;
  useLayoutEffect(() => {
    placedRef.current?.(open && pos !== null);
  }, [open, pos]);

  // Re-place when the content changes size after it was measured — a context menu that swaps its
  // quick-duration chips for the release panel at the same point, a list that filters down. Through
  // a frame, not inline: a re-place can change this very element's size (the `maxHeight`), and a
  // size change made inside a ResizeObserver callback is the loop the browser logs an error for.
  useEffect(() => {
    const el = popRef.current;
    if (!open || !el || typeof ResizeObserver === 'undefined') return;
    let frame = 0;
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(place);
    });
    ro.observe(el);
    return () => {
      ro.disconnect();
      cancelAnimationFrame(frame);
    };
  }, [open, place]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: globalThis.MouseEvent) => {
      const t = e.target as Node;
      if (!anchorRef?.current?.contains(t) && !popRef.current?.contains(t)) onDismiss(false);
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key === 'Escape') onDismiss(true);
    };
    // Capture phase, because scroll does not bubble: the popover is positioned from the trigger's
    // rect, so it has to be re-placed whenever an ancestor scrolls. It must NOT close instead — a
    // trigger inside a scroll container scrolls its own ancestor when the popover takes focus,
    // which closed the menu in the frame it opened.
    const onMove = () => place();
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    window.addEventListener('scroll', onMove, true);
    window.addEventListener('resize', onMove);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
      window.removeEventListener('scroll', onMove, true);
      window.removeEventListener('resize', onMove);
    };
  }, [open, place, anchorRef, onDismiss]);

  if (!open) return null;

  return createPortal(
    <div
      ref={popRef}
      className={['apop', className].filter(Boolean).join(' ')}
      role={role}
      aria-label={label}
      style={
        pos
          ? {
              top: pos.top,
              left: pos.left,
              maxHeight: pos.maxHeight,
              // The surface scrolls only when it was given a ceiling; otherwise its content sets its
              // size and a caller's own scroller inside it keeps working.
              overflowY: pos.maxHeight === undefined ? undefined : 'auto',
            }
          : { top: 0, left: 0, visibility: 'hidden' }
      }
      onKeyDown={onKeyDown}
      // A popover often sits inside a clickable row; a click that reaches the row would navigate
      // away from the control the operator is using.
      onClick={(e) => e.stopPropagation()}
    >
      {children}
    </div>,
    document.body,
  );
}

/** Move focus back to the popover's trigger. Callers that close on Escape use this; the selector is
 *  the same one `AnchoredPopover` measures from, so the two cannot disagree. */
export function focusPopoverTrigger(
  anchor: HTMLElement | null | undefined,
  role: PopoverRole,
): void {
  anchor?.querySelector<HTMLElement>(`[aria-haspopup="${role}"]`)?.focus();
}
