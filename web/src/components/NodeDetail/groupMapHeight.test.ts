// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_GROUP_MAP_PX,
  GROUP_MAP_STEP_PX,
  MIN_GROUP_MAP_PX,
  clampGroupMapHeight,
  groupMapCeiling,
  groupMapHeight,
  groupMapHeightFromDrag,
  groupMapHeightFromKey,
} from './groupMapHeight';

describe('the folder pane map height (ADR-191 Inc.12)', () => {
  it('starts at the height it always had', () => {
    expect(groupMapHeight(null, 900)).toBe(DEFAULT_GROUP_MAP_PX);
  });

  it('holds a stored height inside the window it is read on', () => {
    expect(groupMapHeight(1000, 800)).toBe(720);
    expect(groupMapHeight(5000, 3000)).toBe(1200);
    expect(groupMapHeight(50, 900)).toBe(MIN_GROUP_MAP_PX);
  });

  it('keeps the floor when the window is too short for it, and when it is unmeasured', () => {
    expect(clampGroupMapHeight(500, 150)).toBe(MIN_GROUP_MAP_PX);
    expect(clampGroupMapHeight(150, 0)).toBe(MIN_GROUP_MAP_PX);
    expect(clampGroupMapHeight(5000, 0)).toBe(1200);
    expect(groupMapCeiling(150)).toBe(MIN_GROUP_MAP_PX);
    expect(groupMapCeiling(1000)).toBe(900);
  });

  it('follows the pointer from where the drag began, down to grow', () => {
    expect(groupMapHeightFromDrag(300, 500, 620, 1000)).toBe(420);
    expect(groupMapHeightFromDrag(300, 500, 380, 1000)).toBe(MIN_GROUP_MAP_PX);
  });

  it('steps by key, ArrowDown to grow, and ignores other keys', () => {
    expect(groupMapHeightFromKey(300, 'ArrowDown', 1000)).toBe(300 + GROUP_MAP_STEP_PX);
    expect(groupMapHeightFromKey(300, 'ArrowUp', 1000)).toBe(300 - GROUP_MAP_STEP_PX);
    expect(groupMapHeightFromKey(300, 'ArrowLeft', 1000)).toBeNull();
  });
});
