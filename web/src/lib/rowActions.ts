// SPDX-License-Identifier: AGPL-3.0-only
// How wide a table's row-action column has to be to hold its buttons (ADR-193).
//
// The width used to be a number typed beside each column, and it did not follow the buttons: the
// notification channels kept 96px while their actions grew to four, so "Disable" and "Delete" sat
// past the cell's edge where no click could reach them. Deriving it from the count is what keeps
// the next button from doing the same.

/** `.icon-btn` in `IconButton.css`. A copy of a CSS value — keep the two together. */
const BUTTON_PX = 28;
/** `.ytable-actions { gap }` in `styles/table.css`. A copy of a CSS value. */
const GAP_PX = 4;
/** `.dt-cell { padding: 0 14px }` in `DataTable.css`, both sides. A copy of a CSS value. */
const CELL_PADDING_PX = 2 * 14;

/** The px a `DataTable` actions column needs for `count` buttons, as a column `width`. */
export function rowActionsWidth(count: number): string {
  const n = Math.max(1, Math.floor(count));
  return `${n * BUTTON_PX + (n - 1) * GAP_PX + CELL_PADDING_PX}px`;
}
