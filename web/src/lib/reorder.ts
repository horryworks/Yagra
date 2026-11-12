// SPDX-License-Identifier: AGPL-3.0-only
// The arithmetic of putting an operator-ordered list in a new order (ADR-205 Inc.2, ADR-255). Pure,
// so the drag on every reorderable list (`components/ui/Sortable.tsx`) and the group-aware save on
// Credentials & secrets are the same code and a test reaches both.

/** The list with `active` moved to where `over` stands, the rest keeping their order; null when
 *  nothing moved (a drop on itself, outside the list, or an id the list does not hold). */
export function moveOnto(ids: readonly string[], active: string, over: string | null): string[] | null {
  if (over === null || active === over) return null;
  const from = ids.indexOf(active);
  const to = ids.indexOf(over);
  if (from < 0 || to < 0) return null;
  const out = [...ids];
  out.splice(from, 1);
  out.splice(to, 0, active);
  return out;
}

/**
 * The whole list with one **group** of it put in a new order, every other item where it was.
 *
 * Credentials & secrets reorders one tab at a time, while the order it saves names every
 * credential: the group's slots in `all` are kept, and only which member sits in each changes, so
 * the HTTP and integration rows between two SNMP credentials stay where they stood. `group` is the
 * group's members in their new order; one not in `all` is ignored, and a member `all` holds that
 * `group` leaves out keeps its slot.
 */
export function placeWithinGroup(all: readonly string[], group: readonly string[]): string[] {
  const inAll = new Set(all);
  const ordered = group.filter((id) => inAll.has(id));
  const members = new Set(ordered);
  let next = 0;
  return all.map((x) => (members.has(x) ? ordered[next++] : x));
}
