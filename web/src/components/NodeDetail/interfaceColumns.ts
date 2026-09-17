// SPDX-License-Identifier: AGPL-3.0-only
// The Interfaces list's columns and the grid track each one declares (ADR-129).
//
// WHY THIS FILE EXISTS. This table is not a `DataTable` — its rows are `<button>`s that drive the
// detail dock below it, which is not a shape `DataTable` has (`tabFilters.ts`) — so its template
// lived only in `NodeDetail.css`, as one declaration shared by `.nd-if-head`, `.nd-if-filters` and
// `.nd-if-row`. A width the operator chooses has to come from TypeScript, so the tracks have to be
// nameable here.
//
// 🚨 **The CSS keeps the same list as its fallback, and `interfaceColumns.test.ts` pins the two
// together.** That is a second copy of a fact, which this repo's rules would normally refuse — it
// is kept deliberately, for the reason ADR-074 決定 2 gives: an inline `grid-template-columns`
// beats every media query, and this table is re-laid-out on a phone
// (`html[data-viewport='mobile'] .nd-if-row` turns it into a named 2×2 grid). So the width is
// passed as an inline **custom property** and the CSS keeps its own declaration — which means the
// CSS needs something to fall back to if the property is ever absent, and a single-column grid is
// not an acceptable answer to that. The test is what stops the two drifting.

/** One column of the Interfaces list. `key` is what a stored width is filed under, and for the six
 *  filterable columns it is also the filter key `tabFilters.ts` uses. */
export interface InterfaceColumn {
  key: string;
  /** The CSS grid track, verbatim — this string goes into `--nd-if-cols`. */
  width: string;
}

/**
 * The eleven columns, in the order the header draws them.
 *
 * ⚠️ The order is load-bearing three times over: it is the grid, it is the order of
 * `ColumnFilterRow`'s slots (derived by `filterSlots` below), and it is the index a resize grip is
 * placed at. Inserting a column in the middle also means the header cells' explicit `gridColumn`
 * numbers in `InterfacesTab.tsx` move.
 *
 * The numbers themselves are ADR-126's — measured, not estimated — plus ADR-145's NEIGHBORS and
 * ADR-157's ADDRESSES. The note above the declaration in `NodeDetail.css` explains where each came
 * from; do not change one here without reading it.
 */
export const INTERFACE_COLUMNS: readonly InterfaceColumn[] = [
  { key: 'if_name', width: 'minmax(140px, 1.4fr)' },
  { key: 'if_alias', width: 'minmax(88px, 1.3fr)' },
  { key: 'addresses', width: 'minmax(168px, 1fr)' },
  { key: 'neighbors', width: 'minmax(120px, 1fr)' },
  { key: 'oper', width: '94px' },
  { key: 'media', width: 'minmax(112px, 1fr)' },
  { key: 'speed', width: '84px' },
  { key: 'duplex', width: '76px' },
  { key: 'throughput', width: '132px' },
  { key: 'in', width: 'minmax(74px, 0.7fr)' },
  { key: 'out', width: 'minmax(74px, 0.7fr)' },
];

/**
 * The filter row's slots, one per column in `INTERFACE_COLUMNS` order: the column's key where a
 * filter spec exists for it, `null` where the column carries no control.
 *
 * Derived rather than written out. It used to be a literal in `InterfacesTab.tsx`, guarded only by a
 * test that restated it — and ADR-145 inserting NEIGHBORS as the third column would have slid every
 * control after it one heading to the left while that test still passed.
 */
export function filterSlots(filterKeys: readonly string[]): (string | null)[] {
  const keys = new Set(filterKeys);
  return INTERFACE_COLUMNS.map((c) => (keys.has(c.key) ? c.key : null));
}
