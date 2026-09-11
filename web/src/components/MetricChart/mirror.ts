// SPDX-License-Identifier: AGPL-3.0-only
// The geometry and the wording of a MIRRORED chart — one that plots one direction above zero and
// the other below it (ADR-128). Only the Dashboard's "Interface traffic" widget is one: it is the
// single place in `web/` that negates a series (`interfaceTraffic.ts`'s `sign * v`).
//
// A `.ts` on purpose. `MetricChart.tsx` draws on a canvas, and Vitest runs
// `environment: 'node'` with `include: ['src/**/*.test.ts']` — judgement left in the `.tsx` is
// judgement nothing executes (ADR-052 Inc.6). What is left there is the `ctx` calls; every question
// with an answer that can be wrong is here.

/**
 * The two directions a mirrored chart plots, as the words that go in the axis gutter.
 *
 * ⚠️ **This module does not know which direction belongs on top, and must not learn.** The
 * caller decides, by the sign it gave each series; these two words only have to agree with that
 * choice. Naming a direction here would be a second copy of a decision that lives at the call
 * site — and the Interface traffic widget has already swapped its halves once (ADR-069 増分 2),
 * which a copy here would have quietly contradicted.
 */
export interface MirrorAxis {
  /** Whatever the caller drew above zero. */
  above: string;
  /** Whatever the caller drew below zero. */
  below: string;
}

/** The rotation a gutter label is drawn at, in radians — counter-clockwise, so the text reads
 *  bottom-to-top. Exported because the marks below are picked for how they look *after* it; the
 *  two cannot be changed independently. */
export const LABEL_ROTATION = -Math.PI / 2;

/**
 * The marks that say which way each half runs — **as they appear on the screen, once
 * {@link LABEL_ROTATION} has been applied**.
 *
 * 🚨 **The glyph rotates with the text.** `rotate(-π/2)` maps "right in the text" to "up on the
 * screen", so a `▲` written here renders pointing LEFT. It shipped that way and read `IN ◀` /
 * `OUT ▶` on a real board: two arrows pointing sideways, saying nothing about up and down. Hence
 * the right-pointing glyph above and the left-pointing one below, which is the opposite of how it
 * reads in this file.
 *
 * ⚠️ **And the test written for this pinned the codepoint** (`ABOVE_MARK === '▲'`), so it agreed
 * with the defect and went green for as long as the defect existed. What a test can hold here is
 * that the pair is horizontal, that the two differ, and that the rotation has not changed under
 * them. **Which way they point on screen was settled by looking at a render, and nothing mechanical
 * can re-settle it** — if you change either constant, look at a chart.
 *
 * ⚠️ **Composed here, never carried in the locale files.** Which direction is up is a fact about
 * the chart, not about the language, and a glyph inside a translated string is one nothing can
 * check at all — a locale could ship the pair reversed and every gate would pass.
 */
export const ABOVE_MARK = '▶';
export const BELOW_MARK = '◀';

/** The two strings as they are painted into the gutter. */
export function gutterLabels(axis: MirrorAxis): { above: string; below: string } {
  return {
    above: `${axis.above} ${ABOVE_MARK}`,
    below: `${axis.below} ${BELOW_MARK}`,
  };
}

/** Clear space kept at each end of a rotated gutter label, in canvas pixels. */
export const LABEL_MARGIN = 6;

/**
 * Has this half room to stand its own label in?
 *
 * 🚨 **A rotated label's footprint is its text WIDTH**, so a short chart runs the two into each
 * other — measured at a 60px plot, where they rendered as the single run `OUT ▼IN ▲`. A dashboard
 * cell can be dragged to any height, so this is a normal state, not an edge case. Asked per half so
 * a lopsided window still labels the side that fits; dropping both would lose a name that had room.
 *
 * The width is measured by the caller (`ctx.measureText`) — it depends on the font the canvas
 * resolved, which is not a fact this module can reach.
 */
export function labelFits(halfHeight: number, textWidth: number): boolean {
  if (!Number.isFinite(halfHeight) || !Number.isFinite(textWidth)) return false;
  return halfHeight >= textWidth + LABEL_MARGIN * 2;
}

/** A plot's drawing area in **canvas (device) pixels** — the shape of uPlot's `u.bbox`. */
export interface PlotBox {
  left: number;
  top: number;
  width: number;
  height: number;
}

/** Where the zero rule, the two grounds and the two gutter labels go, all in canvas pixels. */
export interface MirrorLayout {
  /** The y of the zero rule — the boundary the whole feature exists to show. */
  zeroY: number;
  /** The ground under the positive half. */
  above: { y: number; height: number };
  /** The ground under the negative half. */
  below: { y: number; height: number };
  /** Where each rotated gutter label is centred vertically. */
  aboveLabelY: number;
  belowLabelY: number;
}

/**
 * Lay out the two halves around zero.
 *
 * 🚨 **Returns `null` when zero is not strictly inside the plot, and the caller then draws
 * nothing** — no ground, no rule, no labels. A boundary pinned to an edge is a line that says
 * "zero is here" about a place zero is not. `MetricChart`'s `referenceLine` can afford to pin
 * itself to an edge because it marks the clamp with ↑/↓; there is no such mark for an axis, and
 * inventing one would say less than drawing nothing does.
 *
 * With ADR-128's symmetric window zero is always the midpoint, so this is unreachable from the
 * Interface traffic widget. It is reachable from an explicit `yRange`, which wins over the
 * symmetric window — hence the guard rather than an assumption.
 *
 * The edges count as outside: a half with no height cannot be a direction.
 */
export function mirrorLayout(box: PlotBox, zeroY: number): MirrorLayout | null {
  if (!Number.isFinite(zeroY) || !Number.isFinite(box.top) || !Number.isFinite(box.height)) {
    return null;
  }
  const bottom = box.top + box.height;
  if (!(zeroY > box.top) || !(zeroY < bottom)) return null;
  return {
    zeroY,
    above: { y: box.top, height: zeroY - box.top },
    below: { y: zeroY, height: bottom - zeroY },
    aboveLabelY: (box.top + zeroY) / 2,
    belowLabelY: (zeroY + bottom) / 2,
  };
}
