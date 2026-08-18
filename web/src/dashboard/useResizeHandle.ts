// SPDX-License-Identifier: AGPL-3.0-only
// Corner drag-to-resize for a dashboard widget. A self-contained pointer-drag hook (no library):
// it captures the pointer on the grip element (so moves keep flowing even outside the grip and
// unmount mid-drag auto-cleans), measures the grid once on pointer-down, and on each frame snaps
// the pixel delta to the widget's allowed {span, rowSpan} steps (via the pure `snapSize`). The live
// result is exposed as `preview` so the frame can reflow in real time; the final size commits on
// pointer-up. While the pointer is held at the top or bottom edge of the board's scroller it also
// scrolls the board, and measures the drag against the board rather than the window so that motion
// counts — without which a card at the end of the board has nowhere to drag to and can only
// shrink. Arrow keys give the same resize for keyboard users (the dropdowns it replaces were
// keyboard-operable). The math lives in resize.ts; this file is the DOM/React glue.

import { useCallback, useEffect, useRef, useState } from 'react';
import { snapSize, stepAllowed } from './resize';
import type { RowSpan, Span, WidgetDefinition, WidgetInstance } from './types';

const DEFAULT_ROW_PX = 240; // fallback if --mydash-row can't be read
const NO_ROWS: RowSpan[] = []; // stable empty ref for fixed-height widgets (keeps callback deps stable)

/** How close to the scroller's edge the pointer has to be for the board to start moving, in px. */
const EDGE_PX = 48;
/** How far the board moves per frame while the pointer is held in that zone, in px. */
const SCROLL_PX = 12;

interface DragState {
  startX: number;
  startY: number;
  startSpan: number;
  startRow: number;
  colPx: number;
  rowPx: number;
  gapPx: number;
  /** The element that scrolls the board, if any. */
  scroller: HTMLElement | null;
  /** Its scroll offset at pointer-down — the drag is measured against the board, not the window. */
  startScrollTop: number;
}

/** The nearest ancestor that scrolls. On this shell that is `main.shell-content`, not the window:
 *  the document itself never scrolls, so reading `window.scrollY` would always answer 0. Found by
 *  asking the computed style rather than by naming the class, so a future shell keeps working. */
function scrollParent(el: HTMLElement | null): HTMLElement | null {
  for (let n = el?.parentElement ?? null; n; n = n.parentElement) {
    const oy = getComputedStyle(n).overflowY;
    if (oy === 'auto' || oy === 'scroll') return n;
  }
  return null;
}

export interface ResizeHandle {
  handleProps: {
    onPointerDown: (e: React.PointerEvent<HTMLElement>) => void;
    onPointerMove: (e: React.PointerEvent<HTMLElement>) => void;
    onPointerUp: (e: React.PointerEvent<HTMLElement>) => void;
    onPointerCancel: (e: React.PointerEvent<HTMLElement>) => void;
    onKeyDown: (e: React.KeyboardEvent<HTMLElement>) => void;
  };
  /** Live size during a drag (drives the cell's span/rowSpan classes + readout); null when idle. */
  preview: { span: Span; rowSpan: RowSpan } | null;
}

export function useResizeHandle(
  instance: WidgetInstance,
  def: WidgetDefinition,
  setSize: (id: string, span: number, rowSpan: number) => void,
): ResizeHandle {
  const allowedSpans = def.allowedSpans;
  const allowedRowSpans = def.allowedRowSpans ?? NO_ROWS;

  const drag = useRef<DragState | null>(null);
  const latest = useRef<{ x: number; y: number }>({ x: 0, y: 0 });
  const raf = useRef<number | null>(null);
  const [preview, setPreview] = useState<{ span: Span; rowSpan: RowSpan } | null>(null);

  // Cancel a pending frame if the component unmounts mid-drag.
  useEffect(() => () => {
    if (raf.current != null) cancelAnimationFrame(raf.current);
  }, []);

  const snapFromLatest = useCallback((): { span: Span; rowSpan: RowSpan } | null => {
    const d = drag.current;
    if (!d) return null;
    // The vertical delta is measured against the BOARD, not the window: while the pointer sits at
    // the scroller's edge the board moves under it, and a delta in window coordinates would sit
    // still (or, if the card is what moved, count the same travel twice).
    const scrolled = (d.scroller?.scrollTop ?? 0) - d.startScrollTop;
    return snapSize({
      startSpan: d.startSpan,
      startRow: d.startRow,
      dxPx: latest.current.x - d.startX,
      dyPx: latest.current.y - d.startY + scrolled,
      colPx: d.colPx,
      rowPx: d.rowPx,
      gapPx: d.gapPx,
      allowedSpans,
      allowedRowSpans,
    });
  }, [allowedSpans, allowedRowSpans]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      if (e.button !== 0 || !e.isPrimary) return;
      const grip = e.currentTarget;
      const grid = grip.closest('.mydash-grid') as HTMLElement | null;
      if (!grid) return;
      e.stopPropagation(); // don't let dnd-kit / the card see this
      e.preventDefault(); // no text selection / scroll

      const cs = getComputedStyle(grid);
      const gapPx = parseFloat(cs.columnGap) || 0;
      const colPx = (grid.clientWidth - gapPx * 11) / 12; // always 12-based (spans are 12-based)
      const rowPx =
        parseFloat(cs.getPropertyValue('--mydash-row')) || parseFloat(cs.gridAutoRows) || DEFAULT_ROW_PX;

      const scroller = scrollParent(grip);
      drag.current = {
        startX: e.clientX,
        startY: e.clientY,
        startSpan: instance.span,
        startRow: instance.rowSpan ?? 1,
        colPx,
        rowPx,
        gapPx,
        scroller,
        startScrollTop: scroller?.scrollTop ?? 0,
      };
      latest.current = { x: e.clientX, y: e.clientY };
      try {
        grip.setPointerCapture(e.pointerId);
      } catch {
        /* capture is best-effort */
      }
      setPreview({ span: instance.span as Span, rowSpan: (instance.rowSpan ?? 1) as RowSpan });
    },
    [instance.span, instance.rowSpan],
  );

  /** One frame of the drag: nudge the board if the pointer is parked at an edge, then re-snap.
   *
   *  🚨 The nudge is what makes the bottom of a board resizable at all. `onPointerDown` captures
   *  the pointer and calls `preventDefault` — deliberately, or the board would scroll instead of
   *  resizing — so nothing else can bring the room into reach, and a pointer cannot leave the
   *  window. Without this, the last card on a board could only ever be made shorter.
   *
   *  It re-arms itself while the pointer stays in the zone, because a held-still pointer produces
   *  no more events and the scroll has to keep going. The loop ends with the drag. */
  const tick = useCallback(() => {
    raf.current = null;
    const d = drag.current;
    if (!d) return;
    let inZone = false;
    if (d.scroller) {
      const r = d.scroller.getBoundingClientRect();
      const max = d.scroller.scrollHeight - d.scroller.clientHeight;
      if (latest.current.y > r.bottom - EDGE_PX) {
        inZone = true;
        d.scroller.scrollTop = Math.min(max, d.scroller.scrollTop + SCROLL_PX);
      } else if (latest.current.y < r.top + EDGE_PX) {
        inZone = true;
        d.scroller.scrollTop = Math.max(0, d.scroller.scrollTop - SCROLL_PX);
      }
    }
    const next = snapFromLatest();
    if (next) setPreview(next);
    if (inZone) raf.current = requestAnimationFrame(tick);
  }, [snapFromLatest]);

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      if (!drag.current) return;
      latest.current = { x: e.clientX, y: e.clientY };
      if (raf.current != null) return; // coalesce to one update per frame
      raf.current = requestAnimationFrame(tick);
    },
    [tick],
  );

  const endDrag = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      if (!drag.current) return;
      if (raf.current != null) {
        cancelAnimationFrame(raf.current);
        raf.current = null;
      }
      const final = snapFromLatest();
      drag.current = null;
      try {
        e.currentTarget.releasePointerCapture(e.pointerId);
      } catch {
        /* may already be released */
      }
      setPreview(null);
      if (final && (final.span !== instance.span || final.rowSpan !== (instance.rowSpan ?? 1))) {
        setSize(instance.instanceId, final.span, final.rowSpan);
      }
    },
    [snapFromLatest, setSize, instance.instanceId, instance.span, instance.rowSpan],
  );

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLElement>) => {
      let span = instance.span as Span;
      let rowSpan = (instance.rowSpan ?? 1) as RowSpan;
      switch (e.key) {
        case 'ArrowRight':
          span = stepAllowed(span, 1, allowedSpans);
          break;
        case 'ArrowLeft':
          span = stepAllowed(span, -1, allowedSpans);
          break;
        case 'ArrowUp': // taller
          if (allowedRowSpans.length) rowSpan = stepAllowed(rowSpan, 1, allowedRowSpans);
          break;
        case 'ArrowDown': // shorter
          if (allowedRowSpans.length) rowSpan = stepAllowed(rowSpan, -1, allowedRowSpans);
          break;
        default:
          return;
      }
      e.preventDefault();
      if (span !== instance.span || rowSpan !== (instance.rowSpan ?? 1)) {
        setSize(instance.instanceId, span, rowSpan);
      }
    },
    [instance.span, instance.rowSpan, instance.instanceId, allowedSpans, allowedRowSpans, setSize],
  );

  return {
    handleProps: { onPointerDown, onPointerMove, onPointerUp: endDrag, onPointerCancel: endDrag, onKeyDown },
    preview,
  };
}
