// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  clampTreeWidth,
  DEFAULT_TREE_PX,
  DETAIL_MIN_PX,
  HANDLE_PX,
  maxTreeWidth,
  PANE_STEP_PX,
  resolveTreeWidth,
  TREE_MIN_PX,
  widthFromDrag,
  widthFromKey,
} from './nodesPaneWidth';

/** A 1280×720 window with the nav sidebar expanded — the shape the test server is looked at on. */
const CONTAINER_1280 = 1050;

describe('resolveTreeWidth', () => {
  it('gives someone who has never dragged the handle exactly the width the page always had', () => {
    // The accepting case, first on purpose: a clamp that refused everything would satisfy every
    // rejection test below and change what every existing operator sees.
    expect(resolveTreeWidth(null, CONTAINER_1280)).toBe(DEFAULT_TREE_PX);
  });

  it('hands back a stored width unchanged while it still fits', () => {
    expect(resolveTreeWidth(480, CONTAINER_1280)).toBe(480);
  });

  it('re-fits a width dragged out on a wider window', () => {
    // Same browser, half-screened. Without the re-clamp the detail pane would have no room left.
    const stored = maxTreeWidth(1600);
    expect(stored).toBeGreaterThan(maxTreeWidth(900));
    expect(resolveTreeWidth(stored, 900)).toBe(maxTreeWidth(900));
  });

  it('never returns something unusable, however odd the container', () => {
    for (const c of [0, 1, 200, 500, 860, CONTAINER_1280, 2400]) {
      const w = resolveTreeWidth(null, c);
      expect(w).toBeGreaterThanOrEqual(TREE_MIN_PX);
      expect(Number.isFinite(w)).toBe(true);
    }
  });
});

describe('clampTreeWidth', () => {
  it('leaves the detail pane its floor, and counts the handle against the container', () => {
    expect(clampTreeWidth(9999, 1000)).toBe(1000 - HANDLE_PX - DETAIL_MIN_PX);
  });

  it('will not let the tree collapse to nothing', () => {
    // Losing the tree means losing the only way to pick a node; the « rail is the deliberate way
    // to give the detail the whole page, and it is reversible by one button.
    expect(clampTreeWidth(0, 1000)).toBe(TREE_MIN_PX);
    expect(clampTreeWidth(-800, 1000)).toBe(TREE_MIN_PX);
  });

  it('keeps the tree usable on a container too narrow for both floors — the floor is outer', () => {
    // 400px cannot satisfy 220 + 14 + 420. The ceiling goes negative; applying it last would
    // return a tree of -34px. The ordering trap `mapPaneHeight.ts` documents.
    expect(clampTreeWidth(DEFAULT_TREE_PX, 400)).toBe(TREE_MIN_PX);
    expect(clampTreeWidth(9999, 400)).toBe(TREE_MIN_PX);
  });

  it('does not invent a ceiling before the split has been measured', () => {
    expect(clampTreeWidth(DEFAULT_TREE_PX, 0)).toBe(DEFAULT_TREE_PX);
  });

  it('returns whole pixels', () => {
    expect(clampTreeWidth(312.6, CONTAINER_1280)).toBe(313);
  });
});

describe('widthFromDrag', () => {
  it('grows the tree when the pointer moves right', () => {
    // The sign that differs from both vertical handles: this one is a plain addition on clientX.
    expect(widthFromDrag(300, 500, 560, CONTAINER_1280)).toBe(360);
  });

  it('shrinks the tree when the pointer moves left', () => {
    expect(widthFromDrag(300, 500, 440, CONTAINER_1280)).toBe(240);
  });

  it('measures from the gesture origin, so a dropped move event cannot make it creep', () => {
    const direct = widthFromDrag(300, 500, 700, CONTAINER_1280);
    const viaMidpoint = widthFromDrag(300, 500, 600, CONTAINER_1280);
    expect(widthFromDrag(300, 500, 700, CONTAINER_1280)).toBe(direct);
    expect(viaMidpoint).toBe(400);
  });

  it('stays inside the range however far the pointer travels', () => {
    expect(widthFromDrag(300, 500, 5000, CONTAINER_1280)).toBe(maxTreeWidth(CONTAINER_1280));
    expect(widthFromDrag(300, 500, -5000, CONTAINER_1280)).toBe(TREE_MIN_PX);
  });
});

describe('widthFromKey', () => {
  it('grows on ArrowRight and shrinks on ArrowLeft', () => {
    expect(widthFromKey(300, 'ArrowRight', CONTAINER_1280)).toBe(300 + PANE_STEP_PX);
    expect(widthFromKey(300, 'ArrowLeft', CONTAINER_1280)).toBe(300 - PANE_STEP_PX);
  });

  it('claims no other key, so Escape and Tab still reach the page', () => {
    for (const k of ['ArrowUp', 'ArrowDown', 'Escape', 'Tab', 'Enter', ' ', 'a']) {
      expect(widthFromKey(300, k, CONTAINER_1280)).toBeNull();
    }
  });

  it('clamps its step like a drag does', () => {
    expect(widthFromKey(TREE_MIN_PX, 'ArrowLeft', CONTAINER_1280)).toBe(TREE_MIN_PX);
    const max = maxTreeWidth(CONTAINER_1280);
    expect(widthFromKey(max, 'ArrowRight', CONTAINER_1280)).toBe(max);
  });
});
