// SPDX-License-Identifier: AGPL-3.0-only
// The sensors every reorderable surface uses (ADR-255 decision 3): the lists through `Sortable.tsx`,
// and My Dashboard's grid, shared and public boards included. Apart from the component file so that
// file exports components only (fast refresh).

import { KeyboardSensor, MouseSensor, TouchSensor, useSensor, useSensors } from '@dnd-kit/core';
import { sortableKeyboardCoordinates } from '@dnd-kit/sortable';
import { MOUSE_DRAG_DISTANCE_PX, TOUCH_DRAG_DELAY_MS, TOUCH_DRAG_TOLERANCE_PX } from './sortableOrder';

/** A 4px mouse drag, a 250ms touch press (so a swipe still scrolls), and the keyboard. */
export function useSortableSensors() {
  return useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: MOUSE_DRAG_DISTANCE_PX } }),
    useSensor(TouchSensor, {
      activationConstraint: { delay: TOUCH_DRAG_DELAY_MS, tolerance: TOUCH_DRAG_TOLERANCE_PX },
    }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );
}
