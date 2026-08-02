// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  clampPaneHeight,
  defaultPaneHeight,
  heightFromDrag,
  heightFromKey,
  MIN_PANE_PX,
  PANE_STEP_PX,
} from './mapPaneHeight';

describe('defaultPaneHeight', () => {
  it('gives a normal window a pane close to the map’s own 2:1 box', () => {
    // The regression this replaced: `flex: 1` inside a shell whose content area is not a
    // height-constrained flex column collapsed the pane to a ~150px strip, and a fitted world in a
    // 150px-tall, 1200px-wide box is a postage stamp with two-thirds of the pane empty.
    const h = defaultPaneHeight(1080);
    expect(h).toBeGreaterThan(600);
    expect(h).toBeLessThan(1000);
  });

  it('never returns something unusable, however odd the window', () => {
    for (const vp of [0, 1, 200, 400, 768, 1080, 1440, 2160, 4320]) {
      const h = defaultPaneHeight(vp);
      expect(h).toBeGreaterThanOrEqual(MIN_PANE_PX);
      expect(Number.isFinite(h)).toBe(true);
    }
  });
});

describe('clampPaneHeight', () => {
  it('keeps the header and legend on screen', () => {
    // Growing past the window turns the page into a scroll trap where the controls are unreachable.
    expect(clampPaneHeight(99_999, 1000)).toBeLessThanOrEqual(920);
  });

  it('will not let the pane collapse to nothing', () => {
    // Losing the map with no way to get it back is the worse of the two failure directions.
    expect(clampPaneHeight(0, 1000)).toBe(MIN_PANE_PX);
    expect(clampPaneHeight(-500, 1000)).toBe(MIN_PANE_PX);
  });

  it('lets the floor win on a window shorter than the floor', () => {
    // ⚠️ The ordering trap: on a 300px window, 92% is 276 — below the 260 floor is fine, but a
    // window of 200 gives a ceiling of 184, and if the ceiling were applied last the pane would be
    // 184 and then 0 on the next resize. The floor must be the outer bound.
    expect(clampPaneHeight(400, 200)).toBe(MIN_PANE_PX);
    expect(clampPaneHeight(100, 200)).toBe(MIN_PANE_PX);
  });

  it('survives an unmeasured window rather than collapsing', () => {
    expect(clampPaneHeight(500, 0)).toBe(500);
  });
});

describe('heightFromDrag', () => {
  it('follows the pointer', () => {
    expect(heightFromDrag(500, 300, 380, 2000)).toBe(580); // dragged down 80 → 80 taller
    expect(heightFromDrag(500, 300, 220, 2000)).toBe(420); // dragged up 80 → 80 shorter
  });

  it('is computed from the gesture origin, so it cannot drift', () => {
    // The same pointer position must always give the same height, no matter how many move events
    // arrived on the way there. An accumulating implementation drifts when the browser coalesces
    // pointer moves, and the pane slides away from the cursor over a long drag.
    const viaOneJump = heightFromDrag(500, 300, 700, 2000);
    let acc = 500;
    for (let y = 310; y <= 700; y += 10) acc = heightFromDrag(500, 300, y, 2000);
    expect(acc).toBe(viaOneJump);
  });

  it('stays clamped mid-drag', () => {
    expect(heightFromDrag(500, 300, 9999, 1000)).toBeLessThanOrEqual(920);
    expect(heightFromDrag(500, 300, -9999, 1000)).toBe(MIN_PANE_PX);
  });
});

describe('heightFromKey', () => {
  it('is operable without a pointer', () => {
    expect(heightFromKey(500, 1, 2000)).toBe(500 + PANE_STEP_PX);
    expect(heightFromKey(500, -1, 2000)).toBe(500 - PANE_STEP_PX);
  });

  it('obeys the same bounds as a drag', () => {
    expect(heightFromKey(MIN_PANE_PX, -1, 2000)).toBe(MIN_PANE_PX);
    expect(heightFromKey(920, 1, 1000)).toBeLessThanOrEqual(920);
  });
});
