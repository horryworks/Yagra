// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  MOUSE_DRAG_DISTANCE_PX,
  TOUCH_DRAG_DELAY_MS,
  TOUCH_DRAG_TOLERANCE_PX,
  announce,
  verticalOnly,
} from './sortableOrder';

describe('the drag of a reorderable list (ADR-255)', () => {
  it('starts a mouse drag after a few pixels and a touch drag only after a press', () => {
    // A click on the grip stays a click; a swipe to scroll never moves a row (decision 3).
    expect(MOUSE_DRAG_DISTANCE_PX).toBe(4);
    expect(TOUCH_DRAG_DELAY_MS).toBe(250);
    expect(TOUCH_DRAG_TOLERANCE_PX).toBe(5);
  });

  it('moves up and down only', () => {
    expect(verticalOnly({ transform: { x: 30, y: -12, scaleX: 1, scaleY: 1 } })).toEqual({
      x: 0,
      y: -12,
      scaleX: 1,
      scaleY: 1,
    });
  });

  it('reads out the place an item is at, 1-based, as the list stands', () => {
    const ids = ['a', 'b', 'c'];
    const name = (id: string) => id.toUpperCase();
    expect(announce('picked', ids, 'b', null, name)).toEqual({ key: 'picked', name: 'B', place: 2, total: 3 });
    expect(announce('over', ids, 'b', 'a', name)).toMatchObject({ place: 1 });
    expect(announce('dropped', ids, 'a', 'c', name)).toMatchObject({ place: 3 });
    // Dropped outside the list: where it already was.
    expect(announce('cancelled', ids, 'c', 'zz', name)).toMatchObject({ place: 3 });
  });
});
