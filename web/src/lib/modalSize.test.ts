// SPDX-License-Identifier: AGPL-3.0-only
// A resizable dialog's size (ADR-198): the clamp, the drag and the keys, and the registry of
// resizable dialogs against the `<Modal resizeId=…>` call sites that use it.
//
// The call sites are `.tsx`, which Vitest never runs, so the registry half reads source as text —
// the same shape as `tableIds.test.ts`, with a floor so a detector that stopped matching cannot
// pass by inspecting nothing.
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import { sourceFiles } from '../testSupport/sources';
import {
  MIN_MODAL_H,
  MIN_MODAL_W,
  MODAL_RESIZE_IDS,
  MODAL_STEP_PX,
  clampModalHeight,
  clampModalWidth,
  modalHeightFromKey,
  modalSizeFromDrag,
  modalWidthCeiling,
  modalWidthFromKey,
  resolveModalSize,
  withModalSize,
} from './modalSize';

describe('a dialog size, clamped (ADR-198)', () => {
  it('stays inside the window, with the floor as the outer bound', () => {
    expect(clampModalWidth(5000, 1280)).toBe(1232);
    expect(clampModalWidth(100, 1280)).toBe(MIN_MODAL_W);
    // A window narrower than the floor: the ceiling would be below it, and the floor wins.
    expect(clampModalWidth(1000, 300)).toBe(MIN_MODAL_W);
    expect(clampModalHeight(5000, 800)).toBe(704);
    expect(clampModalHeight(10, 800)).toBe(MIN_MODAL_H);
    // An unmeasured window does not collapse the dialog.
    expect(modalWidthCeiling(0)).toBe(MIN_MODAL_W);
  });

  it('re-clamps a stored size for this window, and leaves an untouched axis to the CSS', () => {
    expect(resolveModalSize(undefined, 1280, 800)).toEqual({ w: null, h: null });
    expect(resolveModalSize({ w: 1600, h: null }, 1280, 800)).toEqual({ w: 1232, h: null });
    expect(resolveModalSize({ w: null, h: 300 }, 1280, 800)).toEqual({ w: null, h: 300 });
  });
});

describe('a drag on a centred dialog', () => {
  const viewport = { w: 1920, h: 1080 };
  const start = { w: 960, h: 600 };
  const from = { x: 1440, y: 840 };

  it('grows by twice the pointer travel, so the edge stays under the pointer', () => {
    expect(modalSizeFromDrag('right', start, from, { x: 1540, y: 900 }, viewport)).toEqual({ w: 1160, h: 600 });
    expect(modalSizeFromDrag('bottom', start, from, { x: 1540, y: 900 }, viewport)).toEqual({ w: 960, h: 720 });
    expect(modalSizeFromDrag('corner', start, from, { x: 1540, y: 900 }, viewport)).toEqual({ w: 1160, h: 720 });
  });

  it('is computed from the gesture origin, and clamped', () => {
    expect(modalSizeFromDrag('right', start, from, { x: 1000, y: 840 }, viewport).w).toBe(MIN_MODAL_W);
    expect(modalSizeFromDrag('right', start, from, { x: 3000, y: 840 }, viewport).w).toBe(1872);
  });
});

describe('the keys', () => {
  it('ArrowRight widens and ArrowDown makes taller; other keys belong to the other handle', () => {
    expect(modalWidthFromKey(960, 'ArrowRight', 1920)).toBe(960 + MODAL_STEP_PX);
    expect(modalWidthFromKey(960, 'ArrowLeft', 1920)).toBe(960 - MODAL_STEP_PX);
    expect(modalWidthFromKey(960, 'ArrowDown', 1920)).toBeNull();
    expect(modalHeightFromKey(600, 'ArrowDown', 1080)).toBe(600 + MODAL_STEP_PX);
    expect(modalHeightFromKey(600, 'ArrowUp', 1080)).toBe(600 - MODAL_STEP_PX);
    expect(modalHeightFromKey(600, 'ArrowRight', 1080)).toBeNull();
  });
});

describe('the stored map', () => {
  it('drops a dialog once both axes are back to the default', () => {
    const one = withModalSize({}, 'rca', { w: 1000, h: null });
    expect(one).toEqual({ rca: { w: 1000, h: null } });
    expect(withModalSize(one, 'rca', { w: null, h: null })).toEqual({});
  });
});

const SRC = join(__dirname, '..');
const rel = (p: string) => relative(SRC, p).split('\\').join('/');
const TSX = sourceFiles(SRC, { exts: ['.tsx'], includeTests: true, declarations: true }).filter(
  (f) => !f.includes('.test.'),
);

/** Every `resizeId="…"` in the tree. */
function declaredIds(): { where: string; id: string }[] {
  const out: { where: string; id: string }[] = [];
  for (const file of TSX) {
    const src = readFileSync(file, 'utf8');
    for (const m of src.matchAll(/\bresizeId="([^"]+)"/g)) {
      const line = src.slice(0, m.index ?? 0).split('\n').length;
      out.push({ where: `${rel(file)}:${line}`, id: m[1] as string });
    }
  }
  return out;
}

describe('the resizable-dialog registry against its call sites', () => {
  it('inspected the dialogs this product has', () => {
    // The floor: eight dialogs were made resizable when this shipped.
    expect(TSX.length).toBeGreaterThanOrEqual(150);
    expect(declaredIds().length).toBeGreaterThanOrEqual(MODAL_RESIZE_IDS.length);
  });

  it('uses each name exactly once, and only names the registry declares', () => {
    const known = new Set<string>(MODAL_RESIZE_IDS);
    const byId = new Map<string, string[]>();
    for (const d of declaredIds()) byId.set(d.id, [...(byId.get(d.id) ?? []), d.where]);
    const wrong = [...byId.entries()]
      .filter(([id, wheres]) => !known.has(id) || wheres.length !== 1)
      .map(([id, wheres]) => `"${id}" at ${wheres.join(' and ')}`);
    expect(wrong.sort()).toEqual([]);
    expect(MODAL_RESIZE_IDS.filter((id) => !byId.has(id))).toEqual([]);
  });
});
