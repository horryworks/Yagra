// SPDX-License-Identifier: AGPL-3.0-only
// How wide each column of a table is once the operator has dragged its edge, and what one drag does
// to it (ADR-129).
//
// A `.ts` module because the clamping and the stored-document bookkeeping are the parts that can be
// silently wrong, and Vitest never runs `.tsx` — the arithmetic lives here and the components keep
// only the pointer plumbing. Same split as `pages/nodesPaneWidth.ts`,
// `components/NodeDetail/interfaceDockHeight.ts` and `pages/mapPaneHeight.ts`.
//
// THIS IS THE FOURTH RESIZE HANDLE. Two things differ from the three before it:
//
//  1. **The ceiling does not come from the container.** The other three split a fixed box between
//     two panes, so their ceiling is `container − the other pane's floor`. A table is allowed to
//     grow past its pane — `.dt` has been `overflow-x: auto` since ADR-054 — so the ceiling here is
//     a constant. The floor-is-the-outer-bound ordering is kept anyway: it costs nothing between
//     two constants, and the next person to make the ceiling container-derived would otherwise
//     inherit a clamp that collapses a column to nothing on a narrow window.
//  2. **There are N of them, not one**, so the value is a map rather than a number, and the map has
//     to be bounded — see `MAX_STORED_TABLES`.
//
// The axis and the sign are the same as `nodesPaneWidth.ts` (the other horizontal handle): the grip
// sits on a column's RIGHT edge, so dragging right grows it and ArrowRight grows it. The two
// vertical handles disagree with each other about which key grows; never infer the direction from a
// neighbouring handle.

/**
 * Narrowest a column may be dragged to.
 *
 * Derived, not chosen: `.dt-h` carries `padding: 0 14px` (`DataTable.css`), so 28px of any header
 * cell is padding before a glyph is drawn, and a sortable header adds the sort arrow beside its
 * label. 64px leaves roughly three characters plus the arrow — narrow enough to be useless as a
 * label and wide enough that the column is still visibly there and can be dragged back.
 *
 * ⚠️ Read off the CSS, not measured in a browser. If `.dt-h`'s padding changes, this is the number
 * that silently stops meaning what its name says.
 */
export const COLUMN_MIN_PX = 64;

/**
 * Widest a column may be dragged to.
 *
 * There is no layout reason for a ceiling — the table scrolls — so this exists for two other
 * reasons: a pointer that leaves the window mid-drag must not be able to store an absurd number,
 * and every stored value counts against the account document's 16 KiB (ADR-058). 1200px is wider
 * than any single column on a 1920px screen has cause to be.
 */
export const COLUMN_MAX_PX = 1200;

/** Keyboard resize step, so the handle is operable without a pointer (`ui-conventions.md`). */
export const COLUMN_STEP_PX = 16;

/**
 * How many tables may keep widths at once.
 *
 * ⚠️ **The cap is about the account document, not about this feature.** `PUT /api/v1/preferences`
 * refuses a document over 16 KiB (`MAX_USER_PREFS_BYTES`), the row is one per account, and every
 * other WebUI preference shares that budget. **These two caps are what bound this preference**, and
 * `columnWidths.test.ts` asserts the arithmetic: a document saturated at both caps, with
 * pessimistically long ids, stays under 12 KiB, so the widths can never consume the endpoint's
 * allowance on their own. Realistic use is closer to 2 KB.
 *
 * When the cap is reached the FIRST-inserted table is dropped. Object key order in JS is insertion
 * order for string keys, so "the one the operator touched longest ago" is free; tracking real
 * recency would mean storing a timestamp per table, which is more bytes spent to save bytes.
 * ⚠️ The product has 30 resizable tables and this holds 24 — an operator who tunes every screen
 * loses the ones they tuned first, silently. Accepted: re-dragging restores it, and the failure of
 * a 413 from the endpoint (which is not silent, but *is* unrecoverable without clearing the row)
 * is worse.
 */
export const MAX_STORED_TABLES = 24;

/** How many columns one table may keep.
 *
 *  12 covers every table in the product — Interfaces is the widest at 9 columns, and no `DataTable`
 *  caller declares more than 8 — so this never fires in normal use; it is the second half of the
 *  budget arithmetic above, and a bound on a bug that writes keys in a loop. */
export const MAX_STORED_COLUMNS = 12;

/** Stored widths for one table, keyed by `Column.key`.
 *
 *  ⚠️ **By key, never by index.** The column *set* is not fixed: Settings ▸ API tokens appends its
 *  Actions column only for a caller who may revoke, and `components/EventLog/eventColumns.tsx`
 *  builds a different set for `/events` than for the node-detail Events tab. An index-keyed map
 *  would silently re-point every width the first time a permission changed. */
export type TableColumnWidths = Record<string, number>;

/** Every table's stored widths, keyed by `TableId`. This is the value that rides in the
 *  account-scoped preferences document (ADR-058) and in `prefs.ts`'s local mirror. */
export type ColumnWidthDoc = Record<string, TableColumnWidths>;

/** Column-shaped enough to resolve. `Column<T>` from `DataTable` satisfies this, and so does the
 *  descriptor list `components/NodeDetail/interfaceColumns.ts` holds. */
export interface SizableColumn {
  key: string;
  /** The declared CSS grid track — `'1fr'`, `'120px'`, `'minmax(220px, 2fr)'`. */
  width?: string;
}

/**
 * Hold a width inside the usable range.
 *
 * ⚠️ The floor is the OUTER bound on purpose — the ordering trap `mapPaneHeight.ts` learned first.
 * Between two constants it cannot bite, and that is exactly why it is written this way: the shape
 * has to survive someone later making the ceiling depend on the window.
 */
export function clampColumnWidth(px: number): number {
  return Math.max(COLUMN_MIN_PX, Math.min(COLUMN_MAX_PX, Math.round(px)));
}

/**
 * The width a drag produces: where the column started, plus how far the pointer has moved right.
 *
 * Computed from the gesture's **origin** rather than accumulated per move event, so a coalesced or
 * dropped `pointermove` cannot make the edge creep away from the cursor over a long drag.
 */
export function widthFromDrag(startWidth: number, startClientX: number, clientX: number): number {
  return clampColumnWidth(startWidth + (clientX - startClientX));
}

/**
 * Apply one keyboard step, or `null` when `key` is not one this handle claims.
 *
 * Takes the key name rather than a direction so the axis is decided *here*, where a test can reach
 * it: this handle is horizontal and sits on the column's right edge, so ArrowRight grows.
 */
export function widthFromKey(current: number, key: string): number | null {
  const dir = key === 'ArrowRight' ? 1 : key === 'ArrowLeft' ? -1 : 0;
  if (dir === 0) return null;
  return clampColumnWidth(current + dir * COLUMN_STEP_PX);
}

/**
 * The used width of track `index`, out of a computed `grid-template-columns`.
 *
 * This is how a drag learns what it is starting from: a column declared `1fr` or
 * `minmax(220px, 2fr)` has no pixel width until it is laid out, and the browser resolves every
 * track to `px` in the computed value. Reading the whole row's computed template is one call for
 * any column, which is why no per-cell ref exists.
 *
 * Returns `null` rather than a guess when the value is not a list of pixel lengths — an unrendered
 * grid computes to `none`.
 *
 * ⚠️ Assumes the template carries no `[line-names]`; every template in this product is built by
 * joining plain tracks (`DataTable.tsx`, `interfaceColumns.ts`), so a name would shift the indices.
 */
export function trackPxAt(computed: string, index: number): number | null {
  const raw = computed.trim().split(/\s+/)[index];
  if (raw === undefined) return null;
  const px = /^(\d+(?:\.\d+)?)px$/.exec(raw);
  if (!px) return null;
  const n = Number(px[1]);
  return n > 0 ? n : null;
}

/**
 * The columns as they should be drawn: a stored width becomes a fixed `px` track, everything else
 * keeps the track its author declared.
 *
 * 🚨 **This is the single input every piece of the table's geometry reads.** `DataTable` builds its
 * one shared `grid-template-columns` string from the result AND hands the same result to
 * `lib/tableWidth.ts::minTableWidth`, so the three grids and their common `min-width` cannot
 * disagree about what a column is (ADR-054). Anything that resolves a width a second way is the
 * bug that rule exists to prevent.
 *
 * Returns the array it was given, unchanged, when nothing is overridden — so an operator who has
 * never dragged anything sees byte-identical output to the version before this shipped.
 */
export function resolveWidths<T extends SizableColumn>(
  columns: readonly T[],
  stored: TableColumnWidths | undefined,
): readonly T[] {
  if (!stored || Object.keys(stored).length === 0) return columns;
  let touched = false;
  const out = columns.map((c) => {
    const px = stored[c.key];
    if (px == null || !Number.isFinite(px)) return c;
    touched = true;
    // A fixed pixel track, never `auto`: an `auto` track sizes to its own content, so the header,
    // the filter row and the data rows would resolve one template to three different widths.
    return { ...c, width: `${clampColumnWidth(px)}px` };
  });
  return touched ? out : columns;
}

/**
 * Every track's used width, as a widths map — the state a table enters the moment its operator
 * takes hold of one column.
 *
 * 🚨 **This is what makes "drag one column, the others stay put" true**, and it is not obvious. A
 * grid whose other tracks are `1fr` hands the space back and forth between them: fixing one column
 * wider takes the difference out of the flexible ones and the table never grows, so the operator
 * watches the truncation move one column along instead of going away. Measured on Events: dragging
 * the first column +80px took exactly 80px off the other flexible track. Freezing every column at
 * the width it already had removes the slack there is to take, so the sum grows and the pane
 * scrolls (`minTableWidth` carries the new sum to all three grids, so it scrolls rather than
 * clipping — ADR-054).
 *
 * ⚠️ **The cost is that the table stops being fluid for that operator.** It was laid out for the
 * window it was frozen in, so a narrower window later scrolls where it used to fit. That is the
 * trade a per-column width *is*, every spreadsheet makes it, and the reset control in the header is
 * the way back.
 *
 * A track that did not resolve to pixels is skipped rather than guessed.
 */
export function freezeTracks(computed: string, keys: readonly string[]): TableColumnWidths {
  const out: TableColumnWidths = {};
  keys.forEach((key, i) => {
    const px = trackPxAt(computed, i);
    if (px != null) out[key] = clampColumnWidth(px);
  });
  return out;
}

/**
 * What to call one column's resize grip.
 *
 * Three sources in order: the caller's label map, the column's own heading when that is a plain
 * string, and the storage key as the last resort. It lives here rather than in the component
 * because `.tsx` is never executed by Vitest, and this is a judgement with a failure mode nobody
 * would see: a grip announced as `if_alias` instead of "Description" is a control an operator
 * driving the table from a screen reader cannot place. The key is still better than an empty name.
 *
 * `header` is `unknown` rather than `ReactNode` on purpose — it keeps this module free of React, so
 * a node-environment test can import it.
 */
export function resizeHandleLabel(
  key: string,
  header: unknown,
  labels: Record<string, string> | undefined,
): string {
  const fromLabels = labels?.[key];
  if (fromLabels) return fromLabels;
  return typeof header === 'string' && header.length > 0 ? header : key;
}

/** This table's stored widths, or `undefined` when it has none. */
export function widthsFor(
  doc: ColumnWidthDoc | undefined,
  tableId: string,
): TableColumnWidths | undefined {
  const t = doc?.[tableId];
  return t && Object.keys(t).length > 0 ? t : undefined;
}

/** Whether this table has anything to reset. The reset control is drawn only when this is true. */
export function hasOverrides(doc: ColumnWidthDoc | undefined, tableId: string): boolean {
  return widthsFor(doc, tableId) !== undefined;
}

/**
 * Record several columns' widths at once, returning a new document.
 *
 * This is what one gesture writes: `freezeTracks` produces the widths every column had when the
 * drag began, the dragged one is laid over it, and the whole thing goes in as **one** store update
 * — and therefore one `PUT` and one audit row (ADR-058), which is the rule for this family of
 * handles.
 */
export function setWidths(
  doc: ColumnWidthDoc,
  tableId: string,
  widths: TableColumnWidths,
): ColumnWidthDoc {
  const merged: TableColumnWidths = { ...(doc[tableId] ?? {}) };
  for (const [key, px] of Object.entries(widths)) {
    if (Number.isFinite(px)) merged[key] = clampColumnWidth(px);
  }
  if (Object.keys(merged).length === 0) return doc;
  const next: ColumnWidthDoc = { ...doc, [tableId]: prune(merged, MAX_STORED_COLUMNS, undefined) };
  return prune(next, MAX_STORED_TABLES, tableId);
}

/** Forget one column's width (the double-click), dropping the table when it was the last one. */
export function clearColumn(
  doc: ColumnWidthDoc,
  tableId: string,
  columnKey: string,
): ColumnWidthDoc {
  const table = doc[tableId];
  if (!table || table[columnKey] == null) return doc;
  const rest = { ...table };
  delete rest[columnKey];
  return Object.keys(rest).length === 0 ? clearTable(doc, tableId) : { ...doc, [tableId]: rest };
}

/** Forget every column's width for one table (the reset control). */
export function clearTable(doc: ColumnWidthDoc, tableId: string): ColumnWidthDoc {
  if (!doc[tableId]) return doc;
  const next = { ...doc };
  delete next[tableId];
  return next;
}

/**
 * Read a widths document out of whatever the server returned.
 *
 * Defensive because the preferences document is **opaque to the backend** — it validates only that
 * the body is a JSON object (ADR-058), so a value of the wrong type is a thing this function must
 * survive rather than a thing the API prevents. Anything unrecognised is dropped silently; there is
 * no error to show an operator whose stored widths came back malformed.
 */
export function adoptWidths(raw: unknown): ColumnWidthDoc {
  if (raw == null || typeof raw !== 'object' || Array.isArray(raw)) return {};
  const out: ColumnWidthDoc = {};
  for (const [tableId, value] of Object.entries(raw as Record<string, unknown>)) {
    if (value == null || typeof value !== 'object' || Array.isArray(value)) continue;
    const table: TableColumnWidths = {};
    for (const [columnKey, px] of Object.entries(value as Record<string, unknown>)) {
      if (typeof px !== 'number' || !Number.isFinite(px)) continue;
      table[columnKey] = clampColumnWidth(px);
    }
    if (Object.keys(table).length > 0) out[tableId] = prune(table, MAX_STORED_COLUMNS, undefined);
  }
  return prune(out, MAX_STORED_TABLES, undefined);
}

/**
 * Drop first-inserted entries until `record` holds at most `cap`, never dropping `keep`.
 *
 * `keep` is the entry the current gesture just wrote: evicting it would make a drag on the
 * twenty-fifth table look like it did nothing at all, which reads as a broken handle rather than as
 * a full document.
 */
function prune<V>(
  record: Record<string, V>,
  cap: number,
  keep: string | undefined,
): Record<string, V> {
  const keys = Object.keys(record);
  if (keys.length <= cap) return record;
  const out = { ...record };
  for (const k of keys) {
    if (Object.keys(out).length <= cap) break;
    if (k === keep) continue;
    delete out[k];
  }
  return out;
}
