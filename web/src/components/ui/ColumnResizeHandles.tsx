// SPDX-License-Identifier: AGPL-3.0-only
// The grips that let an operator set a table's column widths, and the control that puts them back
// (ADR-129). **One component for every table in the product** — `DataTable`'s `.dt-head` and the
// node-detail Interfaces list's `.nd-if-head` both render this.
//
// WHY ONE COMPONENT. `ui/ColumnFilterRow.tsx` is the precedent and the cautionary tale: five
// screens hand-rolled a filter row, and four of the five had forgotten the visibility gate
// entirely. A grip written twice is a grip the next person fixes once.
//
// HOW IT SITS IN THE GRID. The header row is a CSS grid with exactly one track per column, and it
// shares its template with the filter row and every data row (ADR-054) — so a grip may not be an
// extra track. It is an extra *item*, explicitly placed into the column it belongs to:
//
//   ⚠️ **The header cells must be explicitly placed as well.** Grid auto-placement skips cells that
//   explicitly-placed items already occupy, so leaving the headers to auto-place would push them
//   into an implicit second row the moment the first grip appeared. `DataTable` and `InterfacesTab`
//   both set `gridColumn` on their header cells for this reason, and it is the whole reason the
//   alternative (wrapping each header in a positioned slot) was rejected: that moves `.dt-h`'s
//   padding inward, which is the exact difference ADR-054 traced a 12px grid-line drift to.
//
// A grip is also never a child of a header cell, because a sortable header *is* a `<button>` and a
// focusable control cannot be nested inside one.
import { useLayoutEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { resizeHandleLabel, trackPxAt } from '../../lib/columnWidths';
import type { ColumnResizeControl } from './useColumnWidths';
import './ColumnResizeHandles.css';

/** Column-shaped enough to label a grip. `Column<T>` from `DataTable` satisfies this. */
export interface LabelledColumn {
  key: string;
  header?: ReactNode;
}

interface Props {
  control: ColumnResizeControl;
  /** The columns, in the order the header draws them — the index *is* the grid track. */
  columns: readonly LabelledColumn[];
  /** Accessible names per column key, for the headers that are not plain strings. */
  labels?: Record<string, string>;
}

/**
 * One grip per column, each on that column's right edge.
 *
 * Dragging one grows or shrinks **only its own column** and lets the table get wider; the pane
 * scrolls sideways when it no longer fits (`.dt` has been `overflow-x: auto` since ADR-054). The
 * alternative — taking the width back off the neighbour — was rejected because it moves the
 * truncated text one column along instead of revealing it, which is the whole point of the feature.
 */
export function ColumnResizeHandles({ control, columns, labels }: Props) {
  const { t } = useTranslation();
  // Every column, in header order. A gesture freezes them all so the width it takes comes from the
  // pane rather than from a flexible neighbour — see `freezeTracks`.
  const keys = columns.map((c) => c.key);

  // What the browser actually laid the tracks out at, so every grip can answer `aria-valuenow` —
  // `ui-conventions.md` asks this family for explicit bounds AND a current value, and a column
  // nobody has dragged has no stored pixel width to report.
  //
  // ⚠️ The row arrives as **state**, not a ref: `ref={…}` inside a tree that early-returns is a ref
  // an effect's dependency array cannot follow, which is how the Interfaces dock once measured a
  // budget of 0 and let the list be dragged to nothing (ADR-058). The first grip hands back its own
  // parent, which is the header row by construction — the grips are its children.
  const [rowEl, setRowEl] = useState<HTMLElement | null>(null);
  const [template, setTemplate] = useState('');
  useLayoutEffect(() => {
    if (!rowEl) return undefined;
    const read = () => setTemplate(getComputedStyle(rowEl).gridTemplateColumns);
    read();
    // A `1fr` track changes width without the template string changing, so the window (or the nav
    // rail, or the split handle beside it) moving is what has to trigger a re-read.
    const ro = new ResizeObserver(read);
    ro.observe(rowEl);
    return () => ro.disconnect();
  }, [rowEl]);
  return (
    <>
      {columns.map((c, i) => {
        const name = resizeHandleLabel(c.key, c.header, labels);
        const now = control.widths?.[c.key] ?? trackPxAt(template, i) ?? undefined;
        return (
          <div
            key={`resize-${c.key}`}
            ref={i === 0 ? (el) => setRowEl(el?.parentElement ?? null) : undefined}
            className="colresize"
            // Explicit placement: see this file's header. `gridRow` matters as much as the column —
            // a header row that later grows a second row would otherwise drop the grips into it.
            style={{ gridColumn: i + 1, gridRow: 1 }}
            role="slider"
            tabIndex={0}
            aria-label={t('column.resize', { column: name })}
            aria-orientation="horizontal"
            aria-valuenow={now}
            aria-valuemin={control.min}
            aria-valuemax={control.max}
            title={t('column.resizeHint')}
            onPointerDown={(e) => control.beginDrag(c.key, i, keys, e)}
            onPointerMove={(e) => control.dragTo(e)}
            onPointerUp={(e) => control.endDrag(e)}
            onPointerCancel={(e) => control.endDrag(e)}
            onKeyDown={(e) => control.stepKey(c.key, i, keys, e)}
            onDoubleClick={() => control.resetColumn(c.key)}
          >
            <span className="colresize-grip" aria-hidden="true" />
          </div>
        );
      })}
    </>
  );
}

/**
 * Put every column of this table back to the width its author declared.
 *
 * Drawn **only once something has been dragged**, in the last column of the header row. A grip's
 * double-click already resets one column, but a gesture with no on-screen affordance fixes "cannot
 * operate", not "cannot find" (`ui-conventions.md` R6, and ADR-073's ✕ is the standing example of
 * settling for the first). Showing it only when there is something to undo keeps the header of an
 * untouched table byte-identical to what it has always been.
 *
 * ⚠️ It is an item in the **last** track, never a track of its own: `.dt-filters` renders one child
 * per column, so a fourth track would slide every filter control out from under its heading — the
 * reason `ClearFilters` lives in the action row rather than in the filter row.
 */
export function ColumnWidthReset({
  control,
  columnCount,
}: {
  control: ColumnResizeControl;
  columnCount: number;
}) {
  const { t } = useTranslation();
  if (!control.overridden || columnCount === 0) return null;
  return (
    <button
      type="button"
      className="colresize-reset"
      style={{ gridColumn: columnCount, gridRow: 1 }}
      title={t('column.resetWidths')}
      aria-label={t('column.resetWidthsAria')}
      onClick={control.resetAll}
    >
      <span aria-hidden="true">⤢</span>
    </button>
  );
}
