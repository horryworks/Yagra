// SPDX-License-Identifier: AGPL-3.0-only
// The popover placement arithmetic (ADR-124 Inc.2). The point half is new; the trigger half is
// pinned so that moving it out of `AnchoredPopover` changed nothing for the callers that open from
// a button.
import { describe, expect, it } from 'vitest';
import { EDGE, GAP, placeFromPoint, placeFromTrigger, samePlacement } from './popoverPlacement';

const vp = { width: 1280, height: 720 };

describe('placeFromTrigger', () => {
  const t = { top: 100, bottom: 120, left: 500, right: 600 };
  const m = { width: 200, height: 100 };

  it('hangs below the trigger, aligned to the chosen edge', () => {
    expect(placeFromTrigger(t, m, vp, 'end')).toEqual({ top: 120 + GAP, left: 400 });
    expect(placeFromTrigger(t, m, vp, 'start')).toEqual({ top: 120 + GAP, left: 500 });
  });

  it('flips above the trigger when below does not fit', () => {
    const low = { ...t, top: 680, bottom: 700 };
    expect(placeFromTrigger(low, m, vp, 'start').top).toBe(680 - 100 - GAP);
  });

  it('keeps EDGE from the left and right', () => {
    expect(placeFromTrigger({ ...t, left: 50, right: 100 }, m, vp, 'end').left).toBe(EDGE);
    expect(placeFromTrigger({ ...t, left: 1200, right: 1250 }, m, vp, 'start').left).toBe(
      1280 - 200 - EDGE,
    );
  });

  it('never imposes a height', () => {
    // A trigger popover that is too tall is an existing condition this file did not change.
    expect(placeFromTrigger(t, { width: 200, height: 2000 }, vp, 'end').maxHeight).toBeUndefined();
  });
});

describe('placeFromPoint', () => {
  const m = { width: 200, height: 300 };

  it('opens down and right of the point when that fits', () => {
    expect(placeFromPoint({ x: 100, y: 100 }, m, vp)).toEqual({ top: 100, left: 100 });
  });

  it('opens upward when there is no room below', () => {
    // 🚨 THE REGRESSION. A right-click near the bottom of the screen used to put the menu's top
    // at the pointer and its bottom off the screen — with the items that act on the whole
    // selection being the ones cut off.
    expect(placeFromPoint({ x: 100, y: 600 }, m, vp)).toEqual({ top: 600 - 300, left: 100 });
  });

  it('opens leftward when there is no room to the right', () => {
    expect(placeFromPoint({ x: 1200, y: 100 }, m, vp)).toEqual({ top: 100, left: 1200 - 200 });
  });

  it('flips both ways from the bottom-right corner', () => {
    expect(placeFromPoint({ x: 1279, y: 719 }, m, vp)).toEqual({ top: 719 - 300, left: 1279 - 200 });
  });

  it('shifts into view when it fits neither below nor above the point', () => {
    // 500 tall, pointer in the middle: 400 + 500 overflows, 400 - 500 is off the top.
    const tall = { width: 200, height: 500 };
    expect(placeFromPoint({ x: 500, y: 400 }, tall, vp)).toEqual({
      top: 720 - 500 - EDGE,
      left: 500,
    });
  });

  it('pins to the top and hands out a maxHeight when taller than the viewport', () => {
    // The scrolling case: a node menu carrying pool chips and suppression sections on a short
    // screen. Without the ceiling the surface has nothing to scroll within.
    const short = { width: 1280, height: 360 };
    expect(placeFromPoint({ x: 100, y: 150 }, { width: 200, height: 450 }, short)).toEqual({
      top: EDGE,
      left: 100,
      maxHeight: 360 - 2 * EDGE,
    });
  });

  it('keeps EDGE from the top-left corner', () => {
    expect(placeFromPoint({ x: 2, y: 2 }, m, vp)).toEqual({ top: EDGE, left: EDGE });
  });
});

describe('samePlacement', () => {
  it('is false before the first measurement, and true only for an identical placement', () => {
    expect(samePlacement(null, { top: 1, left: 2 })).toBe(false);
    expect(samePlacement({ top: 1, left: 2 }, { top: 1, left: 2 })).toBe(true);
    expect(samePlacement({ top: 1, left: 2 }, { top: 1, left: 3 })).toBe(false);
  });

  it('counts the ceiling as part of the placement', () => {
    // Dropping or adding a maxHeight changes the box even when the corner stays put — that is the
    // very change the re-measure exists to make, so it must not be read as "nothing moved".
    expect(samePlacement({ top: 8, left: 2, maxHeight: 344 }, { top: 8, left: 2 })).toBe(false);
    expect(samePlacement({ top: 8, left: 2 }, { top: 8, left: 2, maxHeight: 344 })).toBe(false);
  });
});
