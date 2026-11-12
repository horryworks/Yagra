// SPDX-License-Identifier: AGPL-3.0-only
// The credential picker's selection rule, out here rather than in the component so a test can
// reach it (`tsxJudgement.test.ts` — Vitest never loads a `.tsx`).
//
// ⚠️ Named `credentialSelection`, not `credentialPicker`: a module differing from its component
// only in the case of its first letter resolves to whichever the filesystem feels like on
// Windows, and `tsc` refuses it outright (TS1261).
//
// Since ADR-205 Inc.7 the picker draws every offered credential as ONE list whose rows stay where
// they are: ticking or unticking a row never moves it, and every row can be moved. The selection —
// what is sent, and the order the poller tries it in (ADR-161) — is the ticked rows read top to
// bottom. Before, ticked rows were drawn above the rest, so unticking one sent it to the bottom,
// and only ticked rows had arrows, so a screen that starts with nothing ticked could order nothing.

import { moveBy } from '../../lib/reorder';

/** The shape the rule needs: anything with an id, in the order the picker offers them. */
export interface Selectable {
  id: string;
}

/** `ids` with repeats dropped, first place kept. */
const unique = (ids: readonly string[]): string[] => [...new Set(ids)];

/**
 * The rows the picker draws, top to bottom: every offered credential exactly once.
 *
 * - The first draw (`previous` is null) puts the selection first, in its order, and the rest
 *   after it in the order offered — so what is tried reads from the top.
 * - After that the rows keep their places. The ticked rows' places are filled with `selected` in
 *   its order, which changes nothing when `selected` is the ticked rows read top to bottom (every
 *   change the picker makes itself) and puts an order set from outside ("Select all", a save read
 *   back) where it belongs without moving an unticked row.
 * - A credential newly offered is added at the end; one no longer offered is dropped, and so is a
 *   selected id that is not offered — it could not be sent either.
 */
export function arrangeRows(
  options: readonly Selectable[],
  selected: readonly string[],
  previous: readonly string[] | null,
): string[] {
  const offered = options.map((o) => o.id);
  const offeredSet = new Set(offered);
  const ticked = unique(selected).filter((id) => offeredSet.has(id));
  const tickedSet = new Set(ticked);
  if (previous === null) return [...ticked, ...offered.filter((id) => !tickedSet.has(id))];
  const kept = unique(previous).filter((id) => offeredSet.has(id));
  const keptSet = new Set(kept);
  const rows = [...kept, ...offered.filter((id) => !keptSet.has(id))];
  let next = 0;
  return rows.map((id) => (tickedSet.has(id) ? ticked[next++] : id));
}

/** What is selected, given the rows and which are ticked: the ticked rows read top to bottom. */
export function selectionOf(rows: readonly string[], ticked: ReadonlySet<string>): string[] {
  return rows.filter((id) => ticked.has(id));
}

/** The selection with `id` ticked or unticked. Its row does not move, so neither does any other. */
export function toggleRow(rows: readonly string[], selected: readonly string[], id: string): string[] {
  const ticked = new Set(selected);
  if (!ticked.delete(id)) ticked.add(id);
  return selectionOf(rows, ticked);
}

/** The rows with `id` moved one place up (`-1`) or down (`1`), ticked or not. */
export function moveRow(rows: readonly string[], id: string, delta: -1 | 1): string[] {
  return moveBy(rows, rows.indexOf(id), delta);
}
