// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
//
// These tests drive a **real uPlot instance**. They are not component tests and they do not touch
// geometry: every assertion reads `plot.scales.{x,y}.{min,max}`, which are numbers uPlot computed,
// never a pixel or a laid-out box. The `.tsx` ban (`testing.md`, ADR-052) rests on jsdom having no
// layout engine, and nothing here needs one.
//
// It has to be this way round. The bug these replace was that a range switch did not move the axis,
// and the four tests that stood here for the life of that bug asserted the *shape* of the options
// object — `{ time: true, range: [1000, 2000] }` — which was correct the whole time. Playwright
// cannot see it either: uPlot draws its axis ticks into the canvas, so the window a chart is
// showing is not in the DOM. Running uPlot is the only way to ask.
import { beforeEach, describe, expect, it, vi } from 'vitest';
import uPlot from 'uplot';
import { buildChartScales, type ChartPins } from './scales';

// uPlot calls `matchMedia` while its module body is evaluated (`setPxRatio`), so the stubs have to
// exist before the imports above run — which is what `vi.hoisted` is for; `beforeAll` is too late.
// Measured against jsdom 29.1.1: `Event`, `CustomEvent`, `getComputedStyle` and `devicePixelRatio`
// are all present, and these three are what is missing. They live here rather than in a
// `setupFiles` entry because the other ~200 tests run in `environment: 'node'`, where they mean
// nothing, and a global canvas stub would make a future component test look like it works.
vi.hoisted(() => {
  const g = globalThis as unknown as {
    matchMedia?: unknown;
    Path2D?: unknown;
    HTMLCanvasElement?: { prototype: { getContext: unknown } };
  };
  g.matchMedia = () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
  });
  g.Path2D = class {
    moveTo() {}
    lineTo() {}
    rect() {}
    arc() {}
    closePath() {}
    addPath() {}
  };
  // uPlot only ever issues draw calls on the 2d context — it measures no text (axis sizes come from
  // `axis.size`), so returning a no-op for every property is enough.
  const ctx = new Proxy({}, { get: () => () => {}, set: () => true });
  if (g.HTMLCanvasElement) g.HTMLCanvasElement.prototype.getContext = () => ctx;
});

/** uPlot defers its commit to a microtask, so every assertion follows one turn of the loop. */
const settle = () => new Promise((r) => setTimeout(r, 0));

const HOUR = 3600;
const DAY = 86400;
const T0 = 1_700_000_000;
const ONE_HOUR: [number, number] = [T0 - HOUR, T0];
const SEVEN_DAYS: [number, number] = [T0 - 7 * DAY, T0];

/** A series filling `win`, with values that are neither flat nor monotonic. */
function seriesOver(win: [number, number], value: (i: number) => number = (i) => 10 + (i % 7) * 3) {
  const [from, to] = win;
  const ts: number[] = [];
  const vs: number[] = [];
  const step = Math.max(60, Math.floor((to - from) / 100));
  for (let t = from, i = 0; t <= to; t += step, i++) {
    ts.push(t);
    vs.push(value(i));
  }
  return [ts, vs] as uPlot.AlignedData;
}

const BASE = {
  title: '',
  width: 460,
  height: 220,
  series: [{}, { label: 'v', stroke: '#4c8dd6' }],
};

let host: HTMLDivElement;
beforeEach(() => {
  document.body.innerHTML = '';
  host = document.createElement('div');
  document.body.appendChild(host);
});

/** A chart built the way MetricChart builds one: scales from a getter over the caller's live pins. */
async function chart(pins: ChartPins, data: uPlot.AlignedData) {
  const box = { pins };
  const plot = new uPlot({ ...BASE, scales: buildChartScales(() => box.pins) }, data, host);
  await settle();
  return {
    x: () => [plot.scales.x.min, plot.scales.x.max] as const,
    y: () => [plot.scales.y.min, plot.scales.y.max] as const,
    /** What MetricChart's data effect does: swap the pins, hand over data, never `setScale`. */
    async update(pins: ChartPins, next: uPlot.AlignedData = data) {
      box.pins = pins;
      plot.setData(next);
      await settle();
    },
  };
}

/** A chart built with a raw `scales` option — for the parity twin and the negative control. */
async function raw(scales: uPlot.Options['scales'], data: uPlot.AlignedData) {
  const plot = new uPlot({ ...BASE, ...(scales ? { scales } : {}) }, data, host);
  await settle();
  return plot;
}

describe('buildChartScales', () => {
  it('widens the axis to seven days when the selection does — without rebuilding the instance', async () => {
    const c = await chart({ xRange: ONE_HOUR }, seriesOver(ONE_HOUR));
    expect(c.x()).toEqual([ONE_HOUR[0], ONE_HOUR[1]]);

    await c.update({ xRange: SEVEN_DAYS }, seriesOver(SEVEN_DAYS));

    // The same uPlot instance. Before ADR-117 this stayed one hour wide, which is what "the 7d
    // button does nothing" was.
    expect(c.x()).toEqual([SEVEN_DAYS[0], SEVEN_DAYS[1]]);
  });

  it('advances a relative window with the clock — the same span, half an hour later', async () => {
    const later: [number, number] = [T0 + 1800 - HOUR, T0 + 1800];
    const c = await chart({ xRange: ONE_HOUR }, seriesOver(ONE_HOUR));

    await c.update({ xRange: later }, seriesOver(later));

    // A poll tick slides the window; the axis has to slide with it, or the newest samples fall off
    // the right edge of a chart that looks frozen in time.
    expect(c.x()).toEqual([later[0], later[1]]);
  });

  it('hands the x axis back to the data when the pin is dropped', async () => {
    const data = seriesOver(ONE_HOUR);
    const c = await chart({ xRange: SEVEN_DAYS }, data);
    expect(c.x()).toEqual([SEVEN_DAYS[0], SEVEN_DAYS[1]]);

    await c.update({});

    const ts = data[0] as number[];
    expect(c.x()).toEqual([ts[0], ts[ts.length - 1]]);
  });

  it('applies and releases a y pin — the Bandwidth ⇄ Auto toggle, in both directions', async () => {
    // `interfaceMetrics.ts::throughputBandwidthOverlay` returns `[0, ifSpeedBps]` in capacity mode
    // and `undefined` in auto mode, so releasing the pin is half of what that toggle does. It was
    // the half that did not work: the guarded `setScale('y')` simply never fired.
    const data = seriesOver(ONE_HOUR);
    const c = await chart({ yRange: [0, 1000] }, data);
    expect(c.y()).toEqual([0, 1000]);

    await c.update({});

    const auto = await raw(undefined, data);
    expect(c.y()).toEqual([auto.scales.y.min, auto.scales.y.max]);
  });

  it('lands, unpinned, exactly where uPlot lands with no scales option at all', async () => {
    // This is the only thing guarding the `0.1` copied out of uPlot's unexported `rangePad`.
    // Comparing against `uPlot.rangeNum(…, 0.1, …)` would compare the copy with itself.
    const shapes: [string, uPlot.AlignedData][] = [
      ['an ordinary series', seriesOver(ONE_HOUR)],
      ['a flat series, where the padding is the entire answer', seriesOver(ONE_HOUR, () => 42)],
      ['a single point', [[T0], [42]] as uPlot.AlignedData],
    ];
    for (const [what, data] of shapes) {
      const mine = await chart({}, data);
      const theirs = await raw(undefined, data);
      expect([...mine.x(), ...mine.y()], what).toEqual([
        theirs.scales.x.min,
        theirs.scales.x.max,
        theirs.scales.y.min,
        theirs.scales.y.max,
      ]);
    }
  });

  // 🚨 If this one fails, the bug is not ours: uPlot has changed how it treats an array range, and
  // the reason `buildChartScales` returns functions needs re-reading before anything is "fixed".
  it('is not what a bare [min, max] array does — that freezes the x axis at construction', async () => {
    const plot = await raw({ x: { time: true, range: ONE_HOUR } }, seriesOver(ONE_HOUR));

    plot.setData(seriesOver(SEVEN_DAYS));
    plot.setScale('x', { min: SEVEN_DAYS[0], max: SEVEN_DAYS[1] });
    await settle();

    // Even the explicit `setScale` is overwritten: uPlot re-runs the constant function it wrapped
    // the array in on every scale pass, for the x series specifically.
    expect([plot.scales.x.min, plot.scales.x.max]).toEqual([ONE_HOUR[0], ONE_HOUR[1]]);
  });
});
