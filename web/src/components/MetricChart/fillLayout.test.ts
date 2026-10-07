// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { LEGEND_MIN_CAP, MIN_PLOT_HEIGHT, fillSplit } from './fillLayout';

describe('fillSplit', () => {
  it('gives a short legend all it needs and the plot the rest', () => {
    expect(fillSplit(300, 0, 20)).toEqual({ legendCap: 105, plotHeight: 280 });
  });

  it('caps a tall legend so the plot keeps the larger part (six links, twelve entries)', () => {
    // The measured case: a one-row card about 200px tall, a legend of five wrapped lines.
    const s = fillSplit(200, 0, 110);
    expect(s.legendCap).toBe(70);
    expect(s.plotHeight).toBe(130);
    expect(s.plotHeight).toBeGreaterThan(s.legendCap);
  });

  it('subtracts the title before taking the share', () => {
    expect(fillSplit(220, 20, 500)).toEqual({ legendCap: 70, plotHeight: 130 });
  });

  it('never caps the legend below one line', () => {
    expect(fillSplit(50, 0, 30).legendCap).toBe(LEGEND_MIN_CAP);
  });

  it('keeps the plot floor as the outer bound on a pane too small for both', () => {
    expect(fillSplit(30, 10, 100).plotHeight).toBe(MIN_PLOT_HEIGHT);
    expect(fillSplit(0, 0, 0).plotHeight).toBe(MIN_PLOT_HEIGHT);
  });
});
