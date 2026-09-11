// SPDX-License-Identifier: AGPL-3.0-only
// One table's column widths, and the gesture that changes them (ADR-129).
//
// This module is the *plumbing* — pointer capture, the in-flight width, and which store setter to
// call. Every judgement it makes is a call into `lib/columnWidths.ts`, which is where the tests
// are: a hook cannot run in Vitest's node environment (there is no renderer, and `testing.md` bans
// component tests), so anything decided here would be decided where nothing can reach it.
//
// WHY THE IN-FLIGHT WIDTH LIVES HERE AND NOT IN THE HANDLE. A drag has to be visible while it is
// happening, and what draws it is the *table's* grid template — one string shared by the header,
// the filter row and every data row (ADR-054). So the live value has to be merged into the widths
// the table resolves from, not held inside the grip. The store is written once, on release: each
// write is a PUT and therefore an audit row (ADR-058).
import { useCallback, useMemo, useRef, useState } from 'react';
import type { KeyboardEvent, PointerEvent } from 'react';
import { usePrefsStore } from '../../prefs';
import {
  clearTableColumnWidth,
  clearTableColumnWidths,
  mergeTableColumnWidths,
} from '../../serverPrefs';
import {
  COLUMN_MAX_PX,
  COLUMN_MIN_PX,
  freezeTracks,
  hasOverrides as docHasOverrides,
  trackPxAt,
  widthFromDrag,
  widthFromKey,
  widthsFor,
  type TableColumnWidths,
} from '../../lib/columnWidths';
import type { TableId } from '../../lib/tableIds';

/** What `ColumnResizeHandles` needs in order to drive one table's widths. */
export interface ColumnResizeControl {
  /** Effective widths right now: what is stored, with the in-flight drag laid over it. */
  widths: TableColumnWidths | undefined;
  /** Whether this table has anything to reset — the reset control is drawn only when true. */
  overridden: boolean;
  /** `keys` is every column of this table, in header order — the gesture freezes them all.
   *  See `freezeTracks` for why a drag that moved only its own column would not do what it says. */
  beginDrag(columnKey: string, index: number, keys: readonly string[], e: PointerEvent<HTMLElement>): void;
  dragTo(e: PointerEvent<HTMLElement>): void;
  endDrag(e: PointerEvent<HTMLElement>): void;
  stepKey(
    columnKey: string,
    index: number,
    keys: readonly string[],
    e: KeyboardEvent<HTMLElement>,
  ): void;
  resetColumn(columnKey: string): void;
  resetAll(): void;
  min: number;
  max: number;
}

/** The resolved track widths of the grid a handle lives in, or `[]` when it is not laid out yet. */
function tracksOf(el: HTMLElement | null): string {
  return el ? getComputedStyle(el).gridTemplateColumns : '';
}

/** The header row a handle sits in. The grips are children of it, so no ref is needed. */
function rowOf(e: { currentTarget: HTMLElement }): HTMLElement | null {
  return e.currentTarget.parentElement;
}

export function useColumnWidths(tableId: TableId): ColumnResizeControl {
  const doc = usePrefsStore((s) => s.tableColumnWidths);
  const stored = widthsFor(doc, tableId);

  // The width the pointer is currently describing. Held locally so a drag is visible per frame
  // without writing the store (and therefore the account) per frame.
  // `base` is every column's width as it stood when the gesture began: a drag freezes the whole
  // table so the space it takes comes from the pane rather than from a flexible neighbour.
  const [live, setLive] = useState<{ base: TableColumnWidths; key: string; px: number } | null>(
    null,
  );
  const drag = useRef<{
    base: TableColumnWidths;
    key: string;
    startPx: number;
    startX: number;
  } | null>(null);
  const pointerX = useRef(0);
  const raf = useRef(0);

  const widths = useMemo(() => {
    if (!live) return stored;
    return { ...(stored ?? {}), ...live.base, [live.key]: live.px };
  }, [stored, live]);

  const beginDrag = useCallback(
    (columnKey: string, index: number, keys: readonly string[], e: PointerEvent<HTMLElement>) => {
      const template = tracksOf(rowOf(e));
      const startPx = trackPxAt(template, index);
      // No measurement, no gesture. Starting from a guess would make the column jump to a width it
      // never had the instant the pointer moved one pixel.
      if (startPx == null) return;
      e.currentTarget.setPointerCapture?.(e.pointerId);
      e.preventDefault(); // no text selection dragged across the header
      const base = freezeTracks(template, keys);
      drag.current = { base, key: columnKey, startPx, startX: e.clientX };
      pointerX.current = e.clientX;
      setLive({ base, key: columnKey, px: startPx });
    },
    [],
  );

  const dragTo = useCallback((e: PointerEvent<HTMLElement>) => {
    if (!drag.current) return;
    pointerX.current = e.clientX;
    // Coalesce to one update per frame: pointermove outruns paint, and each update relays out the
    // header, the filter row and every visible data row. Same shape as the other three handles.
    cancelAnimationFrame(raf.current);
    raf.current = requestAnimationFrame(() => {
      const d = drag.current;
      if (!d) return;
      setLive({ base: d.base, key: d.key, px: widthFromDrag(d.startPx, d.startX, pointerX.current) });
    });
  }, []);

  const endDrag = useCallback(
    (e: PointerEvent<HTMLElement>) => {
      const d = drag.current;
      if (!d) return;
      e.currentTarget.releasePointerCapture?.(e.pointerId);
      cancelAnimationFrame(raf.current);
      const final = widthFromDrag(d.startPx, d.startX, pointerX.current);
      drag.current = null;
      setLive(null);
      // One store write — and therefore one PUT and one audit row — per gesture (ADR-058). The
      // frozen base and the dragged column go in together, never as two writes.
      mergeTableColumnWidths(tableId, { ...d.base, [d.key]: final });
    },
    [tableId],
  );

  const stepKey = useCallback(
    (columnKey: string, index: number, keys: readonly string[], e: KeyboardEvent<HTMLElement>) => {
      const template = tracksOf(rowOf(e));
      const current = stored?.[columnKey] ?? trackPxAt(template, index);
      if (current == null) return;
      const next = widthFromKey(current, e.key);
      if (next == null) return; // a key this handle does not claim — let the page have it
      e.preventDefault();
      // Freezes the same way a drag does: an arrow press that only moved its own column would take
      // the pixels off a flexible neighbour instead of widening the table.
      // Discrete, so it is written as it happens; `serverPrefs` coalesces a held key anyway.
      mergeTableColumnWidths(tableId, { ...freezeTracks(template, keys), [columnKey]: next });
    },
    [stored, tableId],
  );

  return {
    widths,
    overridden: docHasOverrides(doc, tableId),
    beginDrag,
    dragTo,
    endDrag,
    stepKey,
    resetColumn: useCallback(
      (columnKey: string) => clearTableColumnWidth(tableId, columnKey),
      [tableId],
    ),
    resetAll: useCallback(() => clearTableColumnWidths(tableId), [tableId]),
    min: COLUMN_MIN_PX,
    max: COLUMN_MAX_PX,
  };
}
