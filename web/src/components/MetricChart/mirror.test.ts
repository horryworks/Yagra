// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  ABOVE_MARK,
  BELOW_MARK,
  LABEL_MARGIN,
  gutterLabels,
  labelFits,
  mirrorLayout,
  type PlotBox,
} from './mirror';

/** A plot box in canvas pixels, the shape uPlot's `u.bbox` has. 200 tall so halves are countable. */
const BOX: PlotBox = { left: 60, top: 10, width: 800, height: 200 };
const BOTTOM = BOX.top + BOX.height; // 210

describe('gutterLabels', () => {
  it('marks the upper half with ▲ and the lower half with ▼', () => {
    const l = gutterLabels({ above: 'IN', below: 'OUT' });
    expect(l.above).toBe(`IN ${ABOVE_MARK}`);
    expect(l.below).toBe(`OUT ${BELOW_MARK}`);
    // The direction is the whole point of the mark, so pin the glyphs themselves too: a swap here
    // renders a chart that names both halves and points them the wrong way, which reads as correct.
    expect(ABOVE_MARK).toBe('▲');
    expect(BELOW_MARK).toBe('▼');
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
  // into the single unreadable run `OUT ▼IN ▲`. A rotated label's footprint is its text WIDTH, and
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
