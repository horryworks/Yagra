// SPDX-License-Identifier: AGPL-3.0-only
// Moving one item of an operator-ordered list by one place (ADR-205 Inc.2). Pure, so the
// credential order on Credentials & secrets and the scan order in the Discovery picker are the
// same arithmetic and a test reaches both.

/** The list with the item at `index` moved one place up (`-1`) or down (`1`). Moving the first
 *  item up or the last item down, or an index outside the list, returns the list unchanged. */
export function moveBy<T>(items: readonly T[], index: number, delta: -1 | 1): T[] {
  const to = index + delta;
  if (index < 0 || index >= items.length || to < 0 || to >= items.length) return [...items];
  const out = [...items];
  [out[index], out[to]] = [out[to], out[index]];
  return out;
}

/**
 * The whole list with `id` moved one place within its **group**, every other item where it was.
 *
 * Credentials & secrets reorders one tab at a time, while the order it saves names every
 * credential: the neighbour an SNMP credential swaps with is the next SNMP credential, wherever the
 * HTTP and integration rows between them stand. The group's slots in `all` are kept; only which
 * member sits in each changes.
 */
export function moveWithinGroup(
  all: readonly string[],
  group: readonly string[],
  id: string,
  delta: -1 | 1,
): string[] {
  const members = all.filter((x) => group.includes(x));
  const moved = moveBy(members, members.indexOf(id), delta);
  let next = 0;
  return all.map((x) => (group.includes(x) ? moved[next++] : x));
}
