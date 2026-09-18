// SPDX-License-Identifier: AGPL-3.0-only
// The credential picker's selection rule, out here rather than in the component so a test can
// reach it (`tsxJudgement.test.ts` — Vitest never loads a `.tsx`).
//
// ⚠️ Named `credentialSelection`, not `credentialPicker`: a module differing from its component
// only in the case of its first letter resolves to whichever the filesystem feels like on
// Windows, and `tsc` refuses it outright (TS1261).

/** The shape the rule needs: anything with an id, in the order the picker offers them. */
export interface Selectable {
  id: string;
}

/**
 * Add or remove `id`, keeping the result in **the order the options are offered in**.
 *
 * 🚨 The ordering is not cosmetic — it is the order the poller tries the credentials in
 * (ADR-161). The picker renders its chips in option order while the array it sent was in click
 * order, so an operator who ticked the third credential first saw "1, 2, 3" and got "3, 1, 2"
 * attempted. Deriving the selection from `options` makes the two agree by construction rather
 * than by two places sorting the same way.
 */
export function toggleSelection(
  options: readonly Selectable[],
  selected: readonly string[],
  id: string,
): string[] {
  if (selected.includes(id)) return selected.filter((x) => x !== id);
  return options.filter((o) => selected.includes(o.id) || o.id === id).map((o) => o.id);
}
