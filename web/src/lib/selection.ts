// SPDX-License-Identifier: AGPL-3.0-only
// Keeping a checkbox selection honest across a reload (ADR-202).

/**
 * The selection, minus every id no longer listed. An id that left the list must not stay selected
 * where nobody can see it, or the next bulk action would carry a row the operator cannot inspect.
 *
 * Returns the same set when nothing dropped, so a caller can hand it to `setState` without causing
 * a render. Which ids count as listed is each screen's own rule, and the caller passes them in.
 */
export function keepListed(
  selected: ReadonlySet<string>,
  listed: Iterable<string>,
): ReadonlySet<string> {
  const present = new Set(listed);
  const kept = [...selected].filter((id) => present.has(id));
  return kept.length === selected.size ? selected : new Set(kept);
}
