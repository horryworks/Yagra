// SPDX-License-Identifier: AGPL-3.0-only
// The chart's two colour/legend decisions, which lived in `MetricChart.tsx` where nothing ran them.
import { describe, expect, it, vi } from 'vitest';
import { applyIdleLegend, resolveColor } from './chartColor';

/** A stand-in computed style: answers only for the variables it was given. */
const style = (vars: Record<string, string>) =>
  ({ getPropertyValue: (k: string) => vars[k] ?? '' }) as unknown as CSSStyleDeclaration;

describe('resolveColor', () => {
  const cs = style({ '--series-1': ' #3ba0ff ', '--empty': '   ' });

  it('resolves exactly `var(--name)` against the computed style', () => {
    expect(resolveColor('var(--series-1)', cs, '#000')).toBe('#3ba0ff');
  });

  it('passes a literal through untouched', () => {
    // A palette constant, a hex, an rgb() — none of them is a variable lookup.
    expect(resolveColor('#ff0000', cs, '#000')).toBe('#ff0000');
    expect(resolveColor('rgb(1, 2, 3)', cs, '#000')).toBe('rgb(1, 2, 3)');
    // Not the exact `var(--x)` shape, so not a lookup either.
    expect(resolveColor('var(--series-1) 50%', cs, '#000')).toBe('var(--series-1) 50%');
  });

  it('falls back rather than painting an empty string', () => {
    // 🚨 This is the case worth having a test for. Canvas renders '' as **black**, so a token that
    // does not resolve on one theme would draw a deliberate-looking, wrong chart instead of an
    // error. The fallback is the caller's palette entry for this series.
    expect(resolveColor('var(--missing)', cs, '#0af')).toBe('#0af');
    expect(resolveColor('var(--empty)', cs, '#0af')).toBe('#0af');
    expect(resolveColor(undefined, cs, '#0af')).toBe('#0af');
    expect(resolveColor('', cs, '#0af')).toBe('#0af');
  });
});

describe('applyIdleLegend', () => {
  /** A stand-in uPlot: `data[0]` is the x axis, the rest are series. */
  const plot = (cursorIdx: number | null, series: (number | null)[][]) => ({
    cursor: { idx: cursorIdx },
    data: [[0, 1, 2], ...series],
    setLegend: vi.fn(),
  });

  it('points the legend at the latest usable sample while the cursor is away', () => {
    const u = plot(null, [[1, 2, 3]]);
    applyIdleLegend(u as never);
    expect(u.setLegend).toHaveBeenCalledTimes(1);
    expect(u.setLegend.mock.calls[0][0]).toEqual({ idx: 2 });
    // `false` = do not fire uPlot's own redraw; the caller is already inside one.
    expect(u.setLegend.mock.calls[0][1]).toBe(false);
  });

  it('does nothing at all while the operator is hovering a point', () => {
    // 🚨 The reason this guard exists: without it, every redraw would snap the reading under the
    // cursor back to the latest sample, so the chart fights the pointer.
    const u = plot(1, [[1, 2, 3]]);
    applyIdleLegend(u as never);
    expect(u.setLegend).not.toHaveBeenCalled();
  });

  it('leaves the legend alone when no sample is usable', () => {
    const u = plot(null, [[null, null, null]]);
    applyIdleLegend(u as never);
    expect(u.setLegend).not.toHaveBeenCalled();
  });

  it('skips the x axis when looking for the latest sample', () => {
    // `data[0]` is timestamps and is always populated; reading it would make every chart look like
    // it had data even when every series was empty.
    const u = plot(null, []);
    applyIdleLegend(u as never);
    expect(u.setLegend).not.toHaveBeenCalled();
  });
});
