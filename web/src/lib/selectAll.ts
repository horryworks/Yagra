// SPDX-License-Identifier: AGPL-3.0-only
// The one judgement behind every "select all" box over a table's rows (ADR-256).
//
// The box is about the rows the operator can see and tick: a row hidden by a column filter, or one
// that draws no checkbox, is neither counted nor selected. Clearing is wider on purpose: it drops
// every selection, hidden rows included, so a row the operator cannot see never rides along on the
// next bulk action after they "cleared" the list.

export type SelectAllState = 'none' | 'some' | 'all';

/** What the box shows for the rows that can be ticked right now. */
export function selectAllState(
  visibleKeys: readonly string[],
  selected: ReadonlySet<string>,
): SelectAllState {
  if (visibleKeys.length === 0) return 'none';
  let on = 0;
  for (const k of visibleKeys) if (selected.has(k)) on += 1;
  if (on === 0) return 'none';
  return on === visibleKeys.length ? 'all' : 'some';
}

/** The selection after a press on the box: everything visible when it was not all ticked,
 *  nothing at all (hidden rows included) when it was. */
export function nextSelection(
  visibleKeys: readonly string[],
  selected: ReadonlySet<string>,
): Set<string> {
  if (visibleKeys.length > 0 && selectAllState(visibleKeys, selected) === 'all') return new Set();
  return new Set([...selected, ...visibleKeys]);
}

/** The selection after one row's box changes. */
export function toggleOne(selected: ReadonlySet<string>, key: string, on: boolean): Set<string> {
  const next = new Set(selected);
  if (on) next.add(key);
  else next.delete(key);
  return next;
}
