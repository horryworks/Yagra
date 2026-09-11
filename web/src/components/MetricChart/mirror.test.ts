// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  ABOVE_MARK,
  BELOW_MARK,
  LABEL_MARGIN,
  LABEL_ROTATION,
  gutterLabels,
  labelFits,
  mirrorLayout,
  type PlotBox,
} from './mirror';

/** A plot box in canvas pixels, the shape uPlot's `u.bbox` has. 200 tall so halves are countable. */
const BOX: PlotBox = { left: 60, top: 10, width: 800, height: 200 };
const BOTTOM = BOX.top + BOX.height; // 210

describe('gutterLabels', () => {
  it('puts one mark on each label, and they are not the same one', () => {
    const l = gutterLabels({ above: 'IN', below: 'OUT' });
    expect(l.above).toBe(`IN ${ABOVE_MARK}`);
    expect(l.below).toBe(`OUT ${BELOW_MARK}`);
    expect(ABOVE_MARK).not.toBe(BELOW_MARK);
  });

  // 🚨 This test's first version asserted `ABOVE_MARK === '▲'` and went green while the chart drew
  // `IN ◀` / `OUT ▶` on a real board — two arrows pointing sideways. The glyph is rotated with the
  // text, so what matters is the direction AFTER `LABEL_ROTATION`, and a codepoint assertion cannot
  // see that: it agreed with the defect for as long as the defect existed
  // (`test-written-from-the-implementation-pins-the-defect`).
  //
  // ⚠️ So what is held here is the *relationship*, not the rendering. `rotate(-π/2)` maps right → up
  // and left → down, so the mark for the upper half is the RIGHT-pointing glyph. The rotation is
  // pinned beside them: change it and this goes red, which is the point at which someone has to
  // look at a chart again. **Nothing mechanical can check where they actually end up pointing.**
  it('uses a horizontal pair, because the rotation turns them a quarter turn', () => {
    expect(LABEL_ROTATION).toBe(-Math.PI / 2);
    expect(ABOVE_MARK).toBe('▶'); // renders pointing UP
    expect(BELOW_MARK).toBe('◀'); // renders pointing DOWN
    // The vertical pair is what was shipped and what read sideways — refuse it by name.
    expect([ABOVE_MARK, BELOW_MARK]).not.toContain('▲');
    expect([ABOVE_MARK, BELOW_MARK]).not.toContain('▼');
  });

  it('carries the caller’s words through untouched, in either language', () => {
    expect(gutterLabels({ above: '受信', below: '送信' })).toEqual({
      above: `受信 ${ABOVE_MARK}`,
      below: `送信 ${BELOW_MARK}`,
    });
  });
});

describe('mirrorLayout', () => {
  it('splits the plot at zero, leaving no gap and no overlap', () => {
    const layout = mirrorLayout(BOX, 110);
    expect(layout).not.toBeNull();
    expect(layout!.zeroY).toBe(110);
    expect(layout!.above).toEqual({ y: 10, height: 100 });
    expect(layout!.below).toEqual({ y: 110, height: 100 });
    // The two grounds together are exactly the plot: a seam would show as a hairline of card
    // colour across the middle, right where the rule is meant to be the only line.
    expect(layout!.above.y).toBe(BOX.top);
    expect(layout!.above.y + layout!.above.height).toBe(layout!.below.y);
    expect(layout!.below.y + layout!.below.height).toBe(BOTTOM);
  });

  it('handles an off-centre zero — the halves follow it rather than staying equal', () => {
    // What an explicit `yRange` can produce. The symmetric window cannot, by construction.
    const layout = mirrorLayout(BOX, 60)!;
    expect(layout.above).toEqual({ y: 10, height: 50 });
    expect(layout.below).toEqual({ y: 60, height: 150 });
  });

  it('centres each label in its own half', () => {
    const layout = mirrorLayout(BOX, 110)!;
    expect(layout.aboveLabelY).toBe(60); // midway between 10 and 110
    expect(layout.belowLabelY).toBe(160); // midway between 110 and 210
  });

  it('follows an off-centre zero with the labels too', () => {
    const layout = mirrorLayout(BOX, 60)!;
    expect(layout.aboveLabelY).toBe(35);
    expect(layout.belowLabelY).toBe(135);
  });

  // 🚨 The refusals. Drawing a boundary at an edge would claim zero is somewhere it is not, and
  // the caller has no ↑/↓ mark to say otherwise — so `null` means "draw nothing", not "clamp".
  it.each([
    ['above the plot', 5],
    ['below the plot', 400],
    ['exactly on the top edge', BOX.top],
    ['exactly on the bottom edge', BOTTOM],
  ])('refuses a zero %s', (_why, zeroY) => {
    expect(mirrorLayout(BOX, zeroY)).toBeNull();
  });

  it('refuses a zero that is not a number', () => {
    expect(mirrorLayout(BOX, NaN)).toBeNull();
    expect(mirrorLayout(BOX, Infinity)).toBeNull();
  });

  it('gives each label a half tall enough to be asked about', () => {
    // The two feed `labelFits`, so they are the numbers the overlap fix turns on.
    const layout = mirrorLayout(BOX, 110)!;
    expect(labelFits(layout.above.height, 40)).toBe(true);
    expect(labelFits(layout.below.height, 40)).toBe(true);
  });

  it('refuses a box with no geometry', () => {
    // uPlot hands a real bbox, but a chart measured before layout can carry NaN, and a NaN
    // rectangle fills nothing while a NaN translate silently drops the label.
    expect(mirrorLayout({ ...BOX, top: NaN }, 110)).toBeNull();
    expect(mirrorLayout({ ...BOX, height: NaN }, 110)).toBeNull();
    expect(mirrorLayout({ ...BOX, height: 0 }, 110)).toBeNull();
  });
});

describe('labelFits', () => {
  // 🚨 Measured on a real render, not imagined: at a 60px plot the two rotated labels ran together
  // into one unreadable run of both names. A rotated label's footprint is its text WIDTH, and
  // a dashboard cell can be dragged to any height, so a short chart is a normal state.
  it('drops a label whose half is shorter than the text is wide', () => {
    expect(labelFits(30, 40)).toBe(false);
    expect(labelFits(40, 40)).toBe(false); // exactly as tall as the text: no clear space at all
  });

  it('keeps one as soon as there is the text plus its margins', () => {
    expect(labelFits(40 + LABEL_MARGIN * 2, 40)).toBe(true);
    expect(labelFits(200, 40)).toBe(true);
  });

  it('asks per half, so a lopsided window still names the side that fits', () => {
    // What an explicit `yRange` produces. Dropping both would lose a name that had the room.
    expect(labelFits(150, 40)).toBe(true);
    expect(labelFits(20, 40)).toBe(false);
  });

  it('refuses a measurement that is not one', () => {
    // `measureText` on a font the canvas could not resolve, or a box measured before layout.
    expect(labelFits(NaN, 40)).toBe(false);
    expect(labelFits(200, NaN)).toBe(false);
    expect(labelFits(Infinity, Infinity)).toBe(false);
  });
});
