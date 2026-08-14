// SPDX-License-Identifier: AGPL-3.0-only
// How much room a `DataTable`'s columns actually need (ADR-054).
//
// WHY THIS EXISTS. `.dt` used to be `overflow: hidden`, so a table whose columns wanted more room
// than the pane gave it **drew the overflow and made it unreachable** — on Settings ▸ API tokens
// the Actions column was 354px past the right edge with no scrollbar to reach it. Worse, CSS grid
// resolves a `1fr` track in an over-constrained grid to the item's min-content contribution, which
// for these cells is **just their padding**: 28px in `.dt-h` (`0 14px`) and 16px in `.dt-f`
// (`0 8px`). Two grids built from the same template string therefore landed their grid lines 12px
// apart, and the first column's filter button came out 14px wide with a 0px label.
//
// The fix is to stop asking the grid to fit: compute the width the columns declare, hand it to all
// three grids as a `min-width`, and let `.dt` scroll horizontally when the pane is narrower. Equal
// `min-width` on all three is what removes the 12px — not a matched value, but no room to differ.
//
// It lives in a `.ts` because `.tsx` is never executed by Vitest (`.claude/rules/testing.md`), and
// "how wide must this table be" is a judgement that has already been wrong once.

/** Column-shaped enough to measure. `Column<T>` from `DataTable` satisfies this. */
export interface MeasurableColumn {
  width?: string;
}

/** Floor for a track that has no declared minimum (`1fr`, `2fr`, or nothing at all).
 *
 *  Not measured and not invented: a name column carries a human-readable name rather than a UUID
 *  (`ui-conventions.md`), real node and token names run 15–20 characters, and the cell must also
 *  hold a sort arrow and — under it — a filter trigger that `ui-conventions.md` floors at 40px.
 *  160px is what leaves all three legible. A column that wants a different floor says so with
 *  `minmax(<floor>, <n>fr)`, which four call sites already did before this function existed. */
export const FLEX_MIN_PX = 160;

const PX = /^(\d+(?:\.\d+)?)px$/;
const MINMAX_PX = /^minmax\(\s*(\d+(?:\.\d+)?)px\s*,/;

/** The narrowest this column can be drawn without losing its content.
 *
 *  ⚠️ **An unrecognised width falls back to `FLEX_MIN_PX`, never to 0.** Returning 0 for a string
 *  this function does not understand is precisely the bug it exists to prevent — the table would go
 *  back to under-reporting its width and clipping the difference, and it would do so silently. */
export function columnMinWidth(width: string | undefined): number {
  if (!width) return FLEX_MIN_PX;
  const trimmed = width.trim();
  const px = PX.exec(trimmed);
  if (px) return Number(px[1]);
  // `minmax(220px, 2fr)` — the first argument is a floor the column's author already chose, so it
  // is a better answer than the generic one. Only a `px` floor is read; `minmax(min-content, …)`
  // has no number to take and falls through to the default below.
  const mm = MINMAX_PX.exec(trimmed);
  if (mm) return Number(mm[1]);
  return FLEX_MIN_PX;
}

/** Total width the columns need. `0` for no columns, so a caller can skip the style entirely.
 *
 *  Deliberately not clamped to any viewport: the value is handed to CSS as a `min-width`, which is
 *  inert while the pane is wider. That is what keeps the 43 tables that already fit byte-identical
 *  — the horizontal scrollbar appears only on a table that was being clipped before. */
export function minTableWidth(columns: readonly MeasurableColumn[]): number {
  return columns.reduce((sum, c) => sum + columnMinWidth(c.width), 0);
}
