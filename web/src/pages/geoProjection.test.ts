// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  clampGeoScale,
  clampGeoView,
  fitPins,
  fitWorld,
  geoBounds,
  MAP_HEIGHT,
  MAP_WIDTH,
  MAX_GEO_SCALE,
  minGeoScale,
  placedOnly,
  project,
  zoomGeoView,
  type GeoView,
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
    const v = fitPins([{ latitude: 10, longitude: 20 }], 800, 400);
    expect(Number.isFinite(v.scale)).toBe(true);
    expect(Number.isFinite(v.tx)).toBe(true);
    expect(Number.isFinite(v.ty)).toBe(true);
    // …and it centres on that pin. (A pin near the edge of the world is pulled off-centre by the
    // clamp instead of showing empty space — that case is under `clampGeoView`.)
    const p = project(10, 20);
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
    // Both well inside the world, so the edge clamp has nothing to correct.
    const pins = [
      { latitude: 48.86, longitude: 2.35 },
      { latitude: 30.04, longitude: 31.24 },
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

/** How far outside the world the pane shows on each side, in px (negative = the map overhangs). */
function exposed(v: GeoView, vw: number, vh: number) {
  return {
    left: v.tx,
    top: v.ty,
    right: vw - (v.tx + MAP_WIDTH * v.scale),
    bottom: vh - (v.ty + MAP_HEIGHT * v.scale),
  };
}

describe('clampGeoScale', () => {
  it('holds every gesture between the pane floor and the ceiling', () => {
    expect(clampGeoScale(0, 800, 400)).toBe(minGeoScale(800, 400));
    expect(clampGeoScale(1e6, 800, 400)).toBe(MAX_GEO_SCALE);
    expect(clampGeoScale(2, 800, 400)).toBe(2);
    // The fit obeys them too, so a pinch cannot leave the map somewhere the fit cannot return it.
    expect(fitWorld(10, 10).scale).toBeGreaterThanOrEqual(minGeoScale(10, 10));
    expect(fitPins([{ latitude: 0, longitude: 0 }], 4000, 4000).scale).toBeLessThanOrEqual(
      MAX_GEO_SCALE,
    );
  });

  it('zooms in ten times further than the old ceiling of 24 (ADR-188)', () => {
    expect(MAX_GEO_SCALE).toBe(240);
  });
});

describe('minGeoScale', () => {
  // ADR-188: zooming out stops where the whole world just fits — never smaller.
  it.each([
    ['a wide pane', 2000, 520],
    ['a tall pane', 400, 800],
  ])('fits the whole world in %s, touching two edges', (_name, vw, vh) => {
    const s = minGeoScale(vw, vh);
    expect(MAP_WIDTH * s).toBeLessThanOrEqual(vw + 1e-9);
    expect(MAP_HEIGHT * s).toBeLessThanOrEqual(vh + 1e-9);
    const fitsWidth = Math.abs(MAP_WIDTH * s - vw) < 1e-9;
    const fitsHeight = Math.abs(MAP_HEIGHT * s - vh) < 1e-9;
    expect(fitsWidth || fitsHeight).toBe(true);
  });

  it('answers a number for an unmeasured pane', () => {
    expect(minGeoScale(0, 0)).toBe(1);
  });
});

describe('clampGeoView', () => {
  const vw = 2000;
  const vh = 520;

  it('brings an over-zoomed-out view back to the whole world, centred', () => {
    const v = clampGeoView({ tx: 5, ty: 5, scale: 0.01 }, vw, vh);
    expect(v.scale).toBe(minGeoScale(vw, vh));
    const e = exposed(v, vw, vh);
    expect(e.left).toBeCloseTo(e.right, 6);
    expect(e.top).toBeCloseTo(0, 6);
    expect(e.bottom).toBeCloseTo(0, 6);
  });

  it('stops at the ceiling', () => {
    expect(clampGeoView({ tx: 0, ty: 0, scale: 1000 }, vw, vh).scale).toBe(MAX_GEO_SCALE);
  });

  it('never drags an edge of a zoomed-in map inside the pane', () => {
    for (const [tx, ty] of [
      [1e6, 1e6],
      [-1e6, -1e6],
      [1e6, -1e6],
      [-1e6, 1e6],
    ]) {
      const e = exposed(clampGeoView({ tx, ty, scale: 20 }, vw, vh), vw, vh);
      expect(e.left).toBeLessThanOrEqual(1e-9);
      expect(e.top).toBeLessThanOrEqual(1e-9);
      expect(e.right).toBeLessThanOrEqual(1e-9);
      expect(e.bottom).toBeLessThanOrEqual(1e-9);
    }
  });

  it('leaves a view that is already inside alone', () => {
    const v = { tx: -1000, ty: -500, scale: 20 };
    expect(clampGeoView(v, vw, vh)).toEqual(v);
  });

  it('does not move the fitted world — Fit is a fixed point of the clamp', () => {
    for (const [w, h] of [
      [2000, 520],
      [400, 800],
      [800, 400],
    ]) {
      const f = fitWorld(w, h);
      const c = clampGeoView(f, w, h);
      expect(c.scale).toBeCloseTo(f.scale, 9);
      expect(c.tx).toBeCloseTo(f.tx, 9);
      expect(c.ty).toBeCloseTo(f.ty, 9);
    }
  });

  it('shows no empty space when fitting a single pin at the edge of the world', () => {
    for (const pin of [
      { latitude: -85, longitude: 179 },
      { latitude: 85, longitude: -179 },
    ]) {
      const e = exposed(fitPins([pin], vw, vh), vw, vh);
      expect(e.left).toBeLessThanOrEqual(1e-9);
      expect(e.top).toBeLessThanOrEqual(1e-9);
      expect(e.right).toBeLessThanOrEqual(1e-9);
      expect(e.bottom).toBeLessThanOrEqual(1e-9);
    }
  });

  it('passes an unmeasured pane through rather than producing NaN', () => {
    const v = { tx: 3, ty: 4, scale: 2 };
    expect(clampGeoView(v, 0, 0)).toEqual(v);
  });
});

describe('zoomGeoView', () => {
  it('keeps the anchor point fixed while zooming in', () => {
    const v = { tx: -1000, ty: -500, scale: 10 };
    const z = zoomGeoView(v, 1.3, 1000, 260, 2000, 520);
    // The map point under (1000, 260) before and after.
    expect((1000 - z.tx) / z.scale).toBeCloseTo((1000 - v.tx) / v.scale, 9);
    expect((260 - z.ty) / z.scale).toBeCloseTo((260 - v.ty) / v.scale, 9);
  });

  it('cannot zoom out past the whole world', () => {
    let v = fitWorld(2000, 520);
    for (let i = 0; i < 20; i++) v = zoomGeoView(v, 1 / 1.3, 0, 0, 2000, 520);
    expect(v).toEqual(fitWorld(2000, 520));
  });
});
