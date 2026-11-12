// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind `Sortable.tsx` (ADR-255), out of the `.tsx` so a test reaches it. Named
// `sortableOrder`, not `sortable`: a module differing from its component only in the case of its
// first letter is refused by `tsc` (TS1261).

/** A mouse drag starts once the pointer has moved this far, so a click on the grip stays a click. */
export const MOUSE_DRAG_DISTANCE_PX = 4;
/** A touch drag starts only after this long a press, so a swipe to scroll the page never moves a
 *  row (decision 3). The same values My Dashboard had before ADR-255 made them everyone's. */
export const TOUCH_DRAG_DELAY_MS = 250;
/** How far a finger may wander during that press before it counts as a scroll instead. */
export const TOUCH_DRAG_TOLERANCE_PX = 5;

/** A list moves up and down only: the drag's sideways part is dropped. Shaped as a dnd-kit
 *  modifier, which takes and returns the drag's `transform`. */
export function verticalOnly<T extends { transform: { x: number; y: number; scaleX: number; scaleY: number } }>({
  transform,
}: T): T['transform'] {
  return { ...transform, x: 0 };
}

/** What the screen reader is told at each step of a drag, as i18n keys under `common:sortable.`
 *  with the item's name and its 1-based place. Null place: the item is not in the list. */
export interface Announcement {
  key: 'picked' | 'over' | 'dropped' | 'cancelled';
  name: string;
  place: number;
  total: number;
}

/** The announcement for one step: `id` is the item moved, `at` the id it now stands on (itself
 *  when it has not moved yet). Its place is where it would land, 1-based. */
export function announce(
  key: Announcement['key'],
  ids: readonly string[],
  id: string,
  at: string | null,
  nameOf: (id: string) => string,
): Announcement {
  const target = at === null ? ids.indexOf(id) : ids.indexOf(at);
  return { key, name: nameOf(id), place: target < 0 ? ids.indexOf(id) + 1 : target + 1, total: ids.length };
}
