import { describe, it, expect } from 'vitest';
import { buildChartScales } from './scales';

describe('buildChartScales', () => {
  it('returns undefined when neither range is given (uPlot auto-fits both axes)', () => {
    expect(buildChartScales()).toBeUndefined();
    expect(buildChartScales(undefined, undefined)).toBeUndefined();
  });

  it('pins the x (time) axis to the requested window so an under-filled range stays visible', () => {
    const s = buildChartScales([1000, 2000]);
    expect(s?.x).toEqual({ time: true, range: [1000, 2000] });
    // y left unset → auto-fit.
    expect(s?.y).toBeUndefined();
  });

  it('pins the y axis alone when only yRange is given (e.g. a 0–100% gauge)', () => {
    const s = buildChartScales(undefined, [0, 100]);
    expect(s?.y).toEqual({ range: [0, 100] });
    expect(s?.x).toBeUndefined();
  });

  it('pins both axes together', () => {
    const s = buildChartScales([1718000000, 1718604800], [0, 100]);
    expect(s?.x).toEqual({ time: true, range: [1718000000, 1718604800] });
    expect(s?.y).toEqual({ range: [0, 100] });
  });
});
