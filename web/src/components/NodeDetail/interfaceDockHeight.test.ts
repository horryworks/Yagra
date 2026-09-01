// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DOCK_MIN_PX,
  DOCK_STEP_PX,
  LIST_MIN_PX,
  clampDockHeight,
  defaultDockHeight,
  dockBudget,
  heightFromDrag,
  heightFromKey,
  resolveDockHeight,
  stickyChromeHeight,
} from './interfaceDockHeight';

/** The dock's own chrome, the number `DOCK_MIN_PX` was derived from. */
const CHROME_PX = 120;
/** What the charts were before issue #65 — the bar this change must not fall under. */
const CHART_HEIGHT_BEFORE = 132;

describe('defaultDockHeight', () => {
  it('opens the charts taller than they were before, on a normal screen', () => {
    // The whole point of issue #65. A 1080p window leaves the tab body roughly 848px.
    const h = defaultDockHeight(848);
    expect(h - CHROME_PX).toBeGreaterThan(CHART_HEIGHT_BEFORE);
  });

  it('never returns something unusable, however odd the container', () => {
    for (const c of [0, 1, 200, 400, 488, 848, 1200, 2400]) {
      const h = defaultDockHeight(c);
      expect(h).toBeGreaterThanOrEqual(DOCK_MIN_PX);
      expect(Number.isFinite(h)).toBe(true);
    }
  });
});

describe('clampDockHeight', () => {
  it('leaves the list its floor on a roomy container', () => {
    expect(clampDockHeight(9999, 1000)).toBe(1000 - LIST_MIN_PX);
  });

  it('will not let the dock collapse to nothing', () => {
    // Losing the charts with no way to get them back is the worse of the two failure directions.
    expect(clampDockHeight(0, 1000)).toBe(DOCK_MIN_PX);
    expect(clampDockHeight(-500, 1000)).toBe(DOCK_MIN_PX);
  });

  it('lets the floor win on a container too short to satisfy both floors', () => {
    // ⚠️ The ordering trap, the same one `pages/mapPaneHeight.ts` documents. On a 300px container
    // the ceiling is 140 — below the 260 floor. If the ceiling were applied last the dock would be
    // 140, and then smaller again on the next resize, until it vanished. The floor must be outside.
    expect(clampDockHeight(400, 300)).toBe(DOCK_MIN_PX);
    expect(clampDockHeight(100, 300)).toBe(DOCK_MIN_PX);
  });

  it('survives an unmeasured container rather than collapsing', () => {
    // First render, before the pane has been measured.
    expect(clampDockHeight(500, 0)).toBe(500);
  });
});

describe('heightFromDrag', () => {
  it('grows when the pointer moves UP', () => {
    // ⚠️ The load-bearing test of this file. The handle sits ABOVE the dock, so this is the
    // opposite of `mapPaneHeight.heightFromDrag` — and a copy-paste from there passes every other
    // test in this file while making the handle move the dock the wrong way.
    expect(heightFromDrag(400, 300, 220, 2000)).toBe(480); // dragged up 80 → 80 taller
    expect(heightFromDrag(400, 300, 380, 2000)).toBe(320); // dragged down 80 → 80 shorter
  });

  it('is computed from the gesture origin, so it cannot drift', () => {
    // The same pointer position must always give the same height, no matter how many move events
    // arrived on the way there. An accumulating implementation drifts when the browser coalesces
    // pointer moves, and the dock slides away from the cursor over a long drag.
    const viaOneJump = heightFromDrag(400, 300, 100, 2000);
    let acc = 400;
    for (let y = 290; y >= 100; y -= 10) acc = heightFromDrag(400, 300, y, 2000);
    expect(acc).toBe(viaOneJump);
  });

  it('stays clamped mid-drag, in both directions', () => {
    expect(heightFromDrag(400, 300, -9999, 1000)).toBe(1000 - LIST_MIN_PX);
    expect(heightFromDrag(400, 300, 9999, 1000)).toBe(DOCK_MIN_PX);
  });
});

describe('heightFromKey', () => {
  it('is operable without a pointer, and ArrowUp grows', () => {
    // ⚠️ Also inverted versus the Geo map, where ArrowDown grows.
    expect(heightFromKey(400, 'ArrowUp', 2000)).toBe(400 + DOCK_STEP_PX);
    expect(heightFromKey(400, 'ArrowDown', 2000)).toBe(400 - DOCK_STEP_PX);
  });

  it('claims no key it does not handle', () => {
    // Returning a number for every key would swallow Tab, Enter and typing into the page.
    for (const key of ['Enter', 'a', 'Tab', 'ArrowLeft', 'ArrowRight', ' ']) {
      expect(heightFromKey(400, key, 2000)).toBeNull();
    }
  });

  it('obeys the same bounds as a drag', () => {
    expect(heightFromKey(DOCK_MIN_PX, 'ArrowDown', 2000)).toBe(DOCK_MIN_PX);
    expect(heightFromKey(1000 - LIST_MIN_PX, 'ArrowUp', 1000)).toBe(1000 - LIST_MIN_PX);
  });
});

describe('resolveDockHeight', () => {
  it('falls back to the default when nothing was ever saved', () => {
    expect(resolveDockHeight(null, 848)).toBe(defaultDockHeight(848));
  });

  it('re-clamps a height saved on a bigger screen', () => {
    // The preference follows the account across machines (ADR-058), so a value dragged out on a
    // 1440p monitor arrives on a laptop. Without the re-clamp it would swallow the whole list.
    expect(resolveDockHeight(1000, 600)).toBe(600 - LIST_MIN_PX);
  });
});

describe('the two floors', () => {
  it('still leave a usable dock in a narrow split pane', () => {
    // A guard on the constants themselves: raising either floor without noticing it swallowed the
    // other would make every container below ~420px degenerate. 460 is a plausible split-pane.
    expect(DOCK_MIN_PX + LIST_MIN_PX).toBeLessThan(460);
  });
});

describe('measuring the chrome, rather than mirroring it', () => {
  /** A stand-in element: `querySelectorAll` answers with the heights it was given. */
  const el = (heights: number[], clientHeight = 0) =>
    ({
      clientHeight,
      querySelectorAll: () => heights.map((height) => ({ getBoundingClientRect: () => ({ height }) })),
    }) as unknown as HTMLElement;

  it('adds up every sticky element it finds', () => {
    expect(stickyChromeHeight(el([32, 34]))).toBe(66);
    expect(stickyChromeHeight(el([32]))).toBe(32);
  });

  it('answers zero when the chrome is hidden', () => {
    // Mobile hides both the header and the filter row. A `display: none` element measures 0, which
    // is exactly the answer wanted — the constant this replaced reserved 32px of nothing there.
    expect(stickyChromeHeight(el([]))).toBe(0);
    expect(stickyChromeHeight(el([0, 0]))).toBe(0);
  });

  it('subtracts the fixed chrome from the pane, not from the window', () => {
    // ⚠️ The pane sits under the shell's top bar, the identity header and the tab strip. Sizing
    // against `window.innerHeight` would let the dock push the list off the bottom.
    expect(dockBudget(el([40], 600))).toBe(560);
    expect(dockBudget(el([40, 24], 600))).toBe(536);
  });

  it('reports the whole pane when nothing is above the list', () => {
    expect(dockBudget(el([], 600))).toBe(600);
  });
});
