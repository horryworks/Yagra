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
import { buildChartScales, symmetricRange, type ChartPins } from './scales';

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

const AXIS = { above: 'IN', below: 'OUT' };

/** A mirrored series: receive above zero, transmit below, `skew`× bigger. The shape ADR-128 is
 *  about — the real reading that started it was 95 Mbps up against 458 Mbps down. */
function mirroredOver(win: [number, number], up: number, down: number) {
  const [from, to] = win;
  const ts: number[] = [];
  const rx: number[] = [];
  const tx: number[] = [];
  const step = Math.max(60, Math.floor((to - from) / 100));
  for (let t = from, i = 0; t <= to; t += step, i++) {
    ts.push(t);
    rx.push(up * (0.8 + 0.2 * ((i % 5) / 4)));
    tx.push(-down * (0.8 + 0.2 * ((i % 7) / 6)));
  }
  return [ts, rx, tx] as uPlot.AlignedData;
}

/** Two series, which a mirrored chart always has: one link's receive and its transmit, in ONE
 *  colour (ADR-069 decision 2) — which is why the side of zero has to carry the direction. */
const MIRROR_SERIES = [{}, { label: 'rx', stroke: '#4c8dd6' }, { label: 'tx', stroke: '#4c8dd6' }];

/** A chart with two series, built the way MetricChart builds one. */
async function mirrorChart(pins: ChartPins, data: uPlot.AlignedData) {
  const box = { pins };
  const plot = new uPlot(
    { ...BASE, series: MIRROR_SERIES, scales: buildChartScales(() => box.pins) },
    data,
    host,
  );
  await settle();
  return {
    y: () => [plot.scales.y.min, plot.scales.y.max] as const,
    async update(pins: ChartPins, next: uPlot.AlignedData = data) {
      box.pins = pins;
      plot.setData(next);
      await settle();
    },
  };
}

describe('symmetricRange', () => {
  // The property the whole of ADR-128 rests on. Everything else about the feature — the rule, the
  // two grounds, the labels — is drawn relative to zero, and all of it is honest only if zero is
  // where the reader expects it.
  it.each([
    ['lopsided the way a real WAN link is', -458e6, 93.9e6],
    ['lopsided the other way', -11e3, 980e6],
    ['balanced', -310e6, 286e6],
    ['one-sided, nothing below zero', 0, 4.2e9],
    ['one-sided, nothing above zero', -4.2e9, 0],
    ['tiny', -3, 1],
  ])('puts zero exactly at the midpoint — %s', (_what, min, max) => {
    const [lo, hi] = symmetricRange(min, max);
    expect(lo).toBe(-hi);
    expect((lo + hi) / 2).toBe(0);
  });

  // The second promise, beside centring: a mirrored chart that clipped its own peak would look
  // entirely healthy. ⚠️ This asserts the PROMISE, not the mechanism — `symmetricRange` falls back
  // to the bare magnitude if `rangeNum` ever returns something short, so a change in uPlot would
  // cost round ticks here rather than turn this red. What would go red is the centring above.
  it('never cuts the data off — the top clears the larger magnitude', () => {
    for (const [min, max] of [
      [-458e6, 93.9e6],
      [-1, 1],
      [0, 7],
      [-99.9, 0],
      [-1e-6, 1e-6],
    ]) {
      const [, hi] = symmetricRange(min, max);
      expect(hi, `${min}..${max}`).toBeGreaterThanOrEqual(Math.max(Math.abs(min), Math.abs(max)));
    }
  });

  it('gives a flat-zero series an axis to draw the rule on', () => {
    // Every sample 0 — an idle port. Without this the window would be [-0, 0] and uPlot would have
    // no scale at all, so the boundary the widget exists to show would have nowhere to go.
    expect(symmetricRange(0, 0)).toEqual([-1, 1]);
  });

  it('refuses to be defined by a value that is not one', () => {
    expect(symmetricRange(NaN, NaN)).toEqual([-1, 1]);
    expect(symmetricRange(-Infinity, Infinity)).toEqual([-1, 1]);
  });
});

describe('buildChartScales, mirrored', () => {
  it('centres zero on a real uPlot instance, not only in the arithmetic', async () => {
    // The symmetric window is DERIVED, so unlike `yRange` it has to survive uPlot's own scale pass
    // rather than being handed straight back. Before ADR-128 this axis came out lopsided —
    // -500M..+200M for exactly this data, with zero a quarter of the way down.
    const c = await mirrorChart({ mirrored: AXIS }, mirroredOver(ONE_HOUR, 93.9e6, 458e6));
    const [lo, hi] = c.y();
    expect(lo).toBe(-hi!);
    expect(hi).toBeGreaterThanOrEqual(458e6);
  });

  it('re-centres as the data moves — the window is not frozen at construction', async () => {
    const c = await mirrorChart({ mirrored: AXIS }, mirroredOver(ONE_HOUR, 10e6, 20e6));
    const first = c.y()[1]!;

    await c.update({ mirrored: AXIS }, mirroredOver(ONE_HOUR, 10e6, 900e6));

    const [lo, hi] = c.y();
    expect(hi).toBeGreaterThan(first); // it grew to hold the new peak…
    expect(lo).toBe(-hi!); // …and stayed centred while doing it
  });

  it('yields to an explicit yRange — the caller’s window outranks the derived one', async () => {
    const data = mirroredOver(ONE_HOUR, 93.9e6, 458e6);
    const c = await mirrorChart({ mirrored: AXIS, yRange: [-100e6, 900e6] }, data);
    expect(c.y()).toEqual([-100e6, 900e6]);
  });

  it('hands the axis back to the data when the mirror is dropped', async () => {
    // The release direction, the way the y pin has one. A widget that stopped declaring itself
    // mirrored must land exactly where an ordinary chart lands, or the two paths have drifted.
    const data = mirroredOver(ONE_HOUR, 93.9e6, 458e6);
    const c = await mirrorChart({ mirrored: AXIS }, data);
    expect(c.y()[0]).toBe(-c.y()[1]!);

    await c.update({});

    // ⚠️ The control has to carry BOTH series. `raw` builds `BASE`'s single one, which would
    // auto-fit over receive alone and compare the released axis against the wrong number.
    const auto = new uPlot({ ...BASE, series: MIRROR_SERIES }, data, host);
    await settle();
    expect(c.y()).toEqual([auto.scales.y.min, auto.scales.y.max]);
  });
});
