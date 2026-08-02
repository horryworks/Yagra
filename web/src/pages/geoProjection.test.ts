// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  clampGeoScale,
  fitPins,
  fitWorld,
  geoBounds,
  MAP_HEIGHT,
  MAP_WIDTH,
  MAX_GEO_SCALE,
  MIN_GEO_SCALE,
  placedOnly,
  project,
} from './geoProjection';

describe('project', () => {
  it('puts known coordinates where the coastline expects them', () => {
    // The whole point of an absolute projection: a coordinate means one place, on every render,
    // regardless of what other sites exist. These are checked against the outline's own grid —
    // if the projection and `worldOutline` ever disagree, pins land in the sea.
    const nullIsland = project(0, 0);
    expect(nullIsland).toEqual({ x: MAP_WIDTH / 2, y: MAP_HEIGHT / 2 });

    // Tokyo, 35.68N 139.69E — right of centre and above it (northern hemisphere).
    const tokyo = project(35.68, 139.69);
    expect(tokyo.x).toBeCloseTo(639.38, 1);
    expect(tokyo.y).toBeCloseTo(108.64, 1);
    expect(tokyo.x).toBeGreaterThan(MAP_WIDTH / 2);
    expect(tokyo.y).toBeLessThan(MAP_HEIGHT / 2);

    // Sydney, 33.87S 151.21E — same side of the world, *below* the equator.
    const sydney = project(-33.87, 151.21);
    expect(sydney.y).toBeGreaterThan(MAP_HEIGHT / 2);
    expect(sydney.x).toBeGreaterThan(tokyo.x);
  });

  it('inverts latitude, because SVG y grows downward', () => {
    // The sign error that would flip the map north-for-south and still look plausible.
    expect(project(90, 0).y).toBe(0);
    expect(project(-90, 0).y).toBe(MAP_HEIGHT);
    expect(project(60, 0).y).toBeLessThan(project(10, 0).y);
  });

  it('spans the full longitude range corner to corner', () => {
    expect(project(0, -180).x).toBe(0);
    expect(project(0, 180).x).toBe(MAP_WIDTH);
  });

  it('clamps out-of-range input rather than letting it drag the view off-screen', () => {
    // A bad row that slipped past the write-side validation draws at the edge. Rejecting it here
    // would mean one bad group blanks the whole map.
    expect(project(0, 999).x).toBe(MAP_WIDTH);
    expect(project(999, 0).y).toBe(0);
    expect(project(-999, -999)).toEqual({ x: 0, y: MAP_HEIGHT });
  });
});

describe('placedOnly', () => {
  it('needs both coordinates, not either', () => {
    // Defaulting a missing half to zero would put the site in the Gulf of Guinea — a real place,
    // confidently wrong, and indistinguishable from a site somebody actually put there.
    const rows = [
      { id: 'a', latitude: 35, longitude: 139 },
      { id: 'b', latitude: 35, longitude: null },
      { id: 'c', latitude: null, longitude: 139 },
      { id: 'd', latitude: null, longitude: null },
      { id: 'e' },
      // 0,0 is a legitimate coordinate and must survive a truthiness-style filter.
      { id: 'f', latitude: 0, longitude: 0 },
    ];
    expect(placedOnly(rows).map((g) => g.id)).toEqual(['a', 'f']);
  });
});

describe('geoBounds', () => {
  it('is null with nothing to bound', () => {
    expect(geoBounds([])).toBeNull();
  });

  it('covers every pin', () => {
    const b = geoBounds([
      { latitude: 35.68, longitude: 139.69 },
      { latitude: -33.87, longitude: 151.21 },
      { latitude: 51.51, longitude: -0.13 },
    ]);
    expect(b).not.toBeNull();
    for (const p of [project(35.68, 139.69), project(-33.87, 151.21), project(51.51, -0.13)]) {
      expect(p.x).toBeGreaterThanOrEqual(b!.minX);
      expect(p.x).toBeLessThanOrEqual(b!.maxX);
      expect(p.y).toBeGreaterThanOrEqual(b!.minY);
      expect(p.y).toBeLessThanOrEqual(b!.maxY);
    }
  });
});

describe('fitPins', () => {
  it('shows the whole world when nothing is placed', () => {
    // An operator who has set no coordinates should see a map and understand what it is for.
    expect(fitPins([], 800, 400)).toEqual(fitWorld(800, 400));
  });

  it('survives a single pin instead of dividing by zero', () => {
    // ⚠️ A zero-width bounding box would make the scale `Infinity` and the translate `NaN`, which
    // renders an empty pane with no error at all — the failure mode that looks like "no data".
    const v = fitPins([{ latitude: 35.68, longitude: 139.69 }], 800, 400);
    expect(Number.isFinite(v.scale)).toBe(true);
    expect(Number.isFinite(v.tx)).toBe(true);
    expect(Number.isFinite(v.ty)).toBe(true);
    // …and it centres on that pin.
    const p = project(35.68, 139.69);
    expect(v.tx + p.x * v.scale).toBeCloseTo(400, 6);
    expect(v.ty + p.y * v.scale).toBeCloseTo(200, 6);
  });

  it('several pins at one place is the same degenerate case', () => {
    const same = [
      { latitude: 10, longitude: 10 },
      { latitude: 10, longitude: 10 },
    ];
    expect(Number.isFinite(fitPins(same, 800, 400).scale)).toBe(true);
  });

  it('centres a spread of pins in the viewport', () => {
    const pins = [
      { latitude: 35.68, longitude: 139.69 },
      { latitude: -33.87, longitude: 151.21 },
    ];
    const v = fitPins(pins, 800, 400);
    const b = geoBounds(pins)!;
    const cx = ((b.minX + b.maxX) / 2) * v.scale + v.tx;
    const cy = ((b.minY + b.maxY) / 2) * v.scale + v.ty;
    expect(cx).toBeCloseTo(400, 6);
    expect(cy).toBeCloseTo(200, 6);
  });

  it('is the identity for an unmeasured pane rather than NaN', () => {
    // A pane rendered before layout has run reports 0×0.
    expect(fitPins([{ latitude: 1, longitude: 1 }], 0, 0)).toEqual({ tx: 0, ty: 0, scale: 1 });
    expect(fitWorld(0, 0)).toEqual({ tx: 0, ty: 0, scale: 1 });
  });
});

describe('clampGeoScale', () => {
  it('holds every gesture inside the bounds a fit could produce', () => {
    expect(clampGeoScale(0)).toBe(MIN_GEO_SCALE);
    expect(clampGeoScale(1e6)).toBe(MAX_GEO_SCALE);
    expect(clampGeoScale(2)).toBe(2);
    // The fit itself obeys them, so a pinch cannot leave the map somewhere the fit can never return
    // it from.
    expect(fitWorld(10, 10).scale).toBeGreaterThanOrEqual(MIN_GEO_SCALE);
    expect(fitPins([{ latitude: 0, longitude: 0 }], 4000, 4000).scale).toBeLessThanOrEqual(
      MAX_GEO_SCALE,
    );
  });
});
