// SPDX-License-Identifier: AGPL-3.0-only
// Corner drag-to-resize for a dashboard widget. A self-contained pointer-drag hook (no library):
// it captures the pointer on the grip element (so moves keep flowing even outside the grip and
// unmount mid-drag auto-cleans), measures the grid once on pointer-down, and on each frame snaps
// the pixel delta to the widget's allowed {span, rowSpan} steps (via the pure `snapSize`). The live
// result is exposed as `preview` so the frame can reflow in real time; the final size commits on
// pointer-up. Arrow keys give the same resize for keyboard users (the dropdowns it replaces were
// keyboard-operable). The math lives in resize.ts; this file is the DOM/React glue.

import { useCallback, useEffect, useRef, useState } from 'react';
import { snapSize, stepAllowed } from './resize';
import type { RowSpan, Span, WidgetDefinition, WidgetInstance } from './types';

const DEFAULT_ROW_PX = 240; // fallback if --mydash-row can't be read
const NO_ROWS: RowSpan[] = []; // stable empty ref for fixed-height widgets (keeps callback deps stable)

interface DragState {
  startX: number;
  startY: number;
  startSpan: number;
  startRow: number;
  colPx: number;
  rowPx: number;
  gapPx: number;
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
    return snapSize({
      startSpan: d.startSpan,
      startRow: d.startRow,
      dxPx: latest.current.x - d.startX,
      dyPx: latest.current.y - d.startY,
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

      drag.current = {
        startX: e.clientX,
        startY: e.clientY,
        startSpan: instance.span,
        startRow: instance.rowSpan ?? 1,
        colPx,
        rowPx,
        gapPx,
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

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLElement>) => {
      if (!drag.current) return;
      latest.current = { x: e.clientX, y: e.clientY };
      if (raf.current != null) return; // coalesce to one update per frame
      raf.current = requestAnimationFrame(() => {
        raf.current = null;
        const next = snapFromLatest();
        if (next) setPreview(next);
      });
    },
    [snapFromLatest],
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
