// SPDX-License-Identifier: AGPL-3.0-only
// Whether the column filter row is drawn (ADR-053 Inc.9). One expression, and it decides for all
// seven filter surfaces — `DataTable`'s `.dt-filters`, `FilterBar` and the five hand-rolled rows.
//
// **Why this is a `.ts` and not just the body of the hook.** The hook lives in a `.tsx`, and a
// `.tsx` test is a file nothing runs (`environment: 'node'` + `include: ['src/**/*.test.ts']`, see
// testing.md). Inc.9 *inverts a default* — the row used to draw unconditionally on a desktop — so
// this one expression is the whole behavioural change, and it has to sit where a test can reach it.

/**
 * True when something is narrowing the list right now.
 *
 * It does two jobs, and they must not come apart: it **forces the row open**, and it **locks the
 * toggle** so the row cannot be hidden while it is on. That pairing is the decision, not a
 * convenience — a list whose rows have been removed with nothing on screen saying which control
 * removed them is the worst shape a filter bug takes, and `EnumFilterSpec.single`'s deletion note
 * in `columnFilter.ts` argues the same point from the other end.
 *
 * It also happens to be what keeps the persisted state a single boolean: there is no fourth state
 * called "closed but filtering", so nothing has to be remembered per screen.
 */
export function filterRowForced(activeCount: number): boolean {
  return activeCount > 0;
}

/**
 * The whole rule: **desktop, and either the operator opened it or something is narrowing the list.**
 *
 * `mode` comes from `useViewportMode()`, so the `uiMode: 'desktop'` preference outranks a narrow
 * window here exactly as it does everywhere else. On mobile the answer is always `false` — the row
 * is replaced by the bottom sheet, and that either/or is stated at `useFilterRowVisible`.
 */
export function filterRowVisible(
  mode: 'mobile' | 'desktop',
  open: boolean,
  activeCount: number,
): boolean {
  if (mode === 'mobile') return false;
  return open || filterRowForced(activeCount);
}

/** One grid track of the row.
 *
 *  `null` is an **empty track**, and it is not padding: the grid is positional, so a column that
 *  simply skipped its cell would slide every later control out from under its header. Say `null`
 *  where a column has no filter — a picture, a live number, an input the operator is typing into. */
export type FilterSlot = string | null | { key: string; align?: 'right' };

export const slotKey = (s: FilterSlot): string | null => (s === null ? null : typeof s === 'string' ? s : s.key);

export const slotAlign = (s: FilterSlot) => (s === null || typeof s === 'string' ? undefined : s.align);
