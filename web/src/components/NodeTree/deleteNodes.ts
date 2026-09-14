// SPDX-License-Identifier: AGPL-3.0-only
// What the bulk-delete dialog says before and after it acts (ADR-124 増分 6). The judgement sits in
// a `.ts` because Vitest does not run `.tsx`; the dialog beside it only renders.

/** How many names the confirmation spells out before summarising the rest. */
export const CONFIRM_NAMES_MAX = 10;

/**
 * The names the confirmation shows, and how many it leaves unnamed.
 *
 * A delete cannot be undone, so the dialog names what it is about to remove rather than only
 * counting it — but a selection of hundreds would push the buttons off the dialog, so past
 * {@link CONFIRM_NAMES_MAX} it says "and N more" instead.
 */
export function namesToConfirm(
  nodes: readonly { name: string }[],
  max = CONFIRM_NAMES_MAX,
): { shown: string[]; more: number } {
  const shown = nodes.slice(0, max).map((n) => n.name);
  return { shown, more: nodes.length - shown.length };
}

/**
 * Whether a bulk delete removed everything it was asked to.
 *
 * `deleted < requested` is not an error — a node can already have gone, or sit in a folder the
 * caller cannot see — but reporting the request as done would claim nodes that are still in the
 * tree. The dialog stays open and says both numbers.
 */
export function deletedEverything(result: { requested: number; deleted: number }): boolean {
  return result.deleted >= result.requested;
}
