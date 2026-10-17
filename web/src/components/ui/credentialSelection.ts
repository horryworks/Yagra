// SPDX-License-Identifier: AGPL-3.0-only
// The credential picker's selection rule, out here rather than in the component so a test can
// reach it (`tsxJudgement.test.ts` — Vitest never loads a `.tsx`).
//
// ⚠️ Named `credentialSelection`, not `credentialPicker`: a module differing from its component
// only in the case of its first letter resolves to whichever the filesystem feels like on
// Windows, and `tsc` refuses it outright (TS1261).

import { moveBy } from '../../lib/reorder';

/** The shape the rule needs: anything with an id, in the order the picker offers them. */
export interface Selectable {
  id: string;
}

/**
 * Add or remove `id`. **The selection is an order, not a set** — it is the order the poller tries
 * the credentials in (ADR-161), and since ADR-205 Inc.2 the operator arranges it. An added
 * credential goes to the end; a removed one closes the gap.
 *
 * 🚨 What the picker draws is this array, in this order, numbered. ADR-161 found the chips drawn in
 * one order while the request carried another; what keeps the two from disagreeing is that the
 * picker renders `selected` itself, never the options filtered by it.
 *
 * Ids no longer offered are dropped whenever anything is ticked — a stale remembered id would fail
 * the whole scan with a 400 rather than costing one credential.
 */
export function toggleSelection(
  options: readonly Selectable[],
  selected: readonly string[],
  id: string,
): string[] {
  if (selected.includes(id)) return selected.filter((x) => x !== id);
  const offered = new Set(options.map((o) => o.id));
  if (!offered.has(id)) return [...selected];
  return [...selected.filter((x) => offered.has(x)), id];
}

/**
 * What the picker draws (ADR-205 Inc.3): every offered credential, split in two.
 *
 * `used` is the selection in its try order — the order it is sent in, so it is the order it is
 * numbered in. `unused` is every other offered credential, in the order offered. An id selected but
 * no longer offered appears in neither: it cannot be sent either (`toggleSelection` drops it).
 */
export function splitSelection<T extends Selectable>(
  options: readonly T[],
  selected: readonly string[],
): { used: T[]; unused: T[] } {
  const byId = new Map(options.map((o) => [o.id, o]));
  const seen = new Set<string>();
  const used: T[] = [];
  for (const id of selected) {
    const o = byId.get(id);
    if (o && !seen.has(id)) {
      seen.add(id);
      used.push(o);
    }
  }
  return { used, unused: options.filter((o) => !seen.has(o.id)) };
}

/** The selection with `id` moved one place earlier (`-1`) or later (`1`) in the try order. */
export function moveSelected(selected: readonly string[], id: string, delta: -1 | 1): string[] {
  return moveBy(selected, selected.indexOf(id), delta);
}
