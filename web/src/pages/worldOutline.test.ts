// SPDX-License-Identifier: AGPL-3.0-only
//
// The coastline is generated data, so what is worth testing is not the numbers but the claim they
// make: **is there land where land is, and ocean where ocean is**. The first version of this file's
// subject was hand-traced and drew continents as blobs — visibly wrong to anyone who looked, and
// caught by a human rather than by anything here. This is what would have caught it.

import { describe, expect, it } from 'vitest';
import { MAP_HEIGHT, MAP_WIDTH, project } from './geoProjection';
import { WORLD_OUTLINE } from './worldOutline';

/** Parse one `M…L…L…Z` path back into points, in map units. */
function ringPoints(path: string): [number, number][] {
  return path
    .slice(1, -1) // drop the leading M and trailing Z
    .split(/[ML]/)
    .filter((s) => s.length > 0)
    .map((pair) => {
      const [x, y] = pair.trim().split(' ').map(Number);
      return [x, y] as [number, number];
    });
}

const RINGS = WORLD_OUTLINE.map(ringPoints);

/** Even-odd point-in-polygon. */
function inRing([px, py]: [number, number], ring: [number, number][]): boolean {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [xi, yi] = ring[i];
    const [xj, yj] = ring[j];
    if (yi > py !== yj > py && px < ((xj - xi) * (py - yi)) / (yj - yi) + xi) {
      inside = !inside;
    }
  }
  return inside;
}

/** Whether the outline says this coordinate is on land. */
function isLand(lat: number, lon: number): boolean {
  const p = project(lat, lon);
  return RINGS.some((r) => inRing([p.x, p.y], r));
}

describe('WORLD_OUTLINE', () => {
  it('puts land under cities and water under oceans', () => {
    // Spread deliberately across every continent and every quadrant of the map, because the failure
    // modes are regional: a sign error flips a hemisphere, an offset shifts one continent, and a
    // bad simplification eats one landmass while leaving the rest correct.
    const cities: [string, number, number][] = [
      ['Tokyo', 35.68, 139.69],
      ['London', 51.51, -0.13],
      ['New York', 40.71, -74.01],
      ['Sydney', -33.87, 151.21],
      ['São Paulo', -23.55, -46.63],
      ['Cairo', 30.04, 31.24],
      ['Johannesburg', -26.2, 28.05],
      ['Denver', 39.74, -104.99],
      ['Moscow', 55.76, 37.62],
      ['Delhi', 28.61, 77.21],
      ['Beijing', 39.9, 116.41],
    ];
    for (const [name, lat, lon] of cities) {
      expect(isLand(lat, lon), `${name} should be on land`).toBe(true);
    }

    const oceans: [string, number, number][] = [
      ['mid-Pacific', 0, -140],
      ['mid-Atlantic', 0, -30],
      ['Indian Ocean', -20, 80],
      ['Southern Ocean', -55, 100],
      ['North Pacific', 40, -170],
    ];
    for (const [name, lat, lon] of oceans) {
      expect(isLand(lat, lon), `${name} should be open water`).toBe(false);
    }
  });

  it('covers Antarctica, which is the ring most easily lost to simplification', () => {
    // Its polygon walks the -90 edge and is the one shape a naive area filter or a bad ring-closing
    // rule drops entirely — leaving a map that looks fine until someone notices the bottom is gone.
    expect(isLand(-80, 0)).toBe(true);
    expect(isLand(-78, 160)).toBe(true);
  });

  it('stays inside the map bounds it is drawn in', () => {
    // A coordinate outside the box means the outline and `geoProjection` disagree about the grid,
    // which puts pins on the wrong part of a coastline that still looks plausible.
    for (const ring of RINGS) {
      for (const [x, y] of ring) {
        expect(Number.isFinite(x) && Number.isFinite(y)).toBe(true);
        expect(x).toBeGreaterThanOrEqual(0);
        expect(x).toBeLessThanOrEqual(MAP_WIDTH);
        expect(y).toBeGreaterThanOrEqual(0);
        expect(y).toBeLessThanOrEqual(MAP_HEIGHT);
      }
    }
  });

  it('is real geometry rather than a handful of blobs', () => {
    // The regression this exists for. A hand-traced outline has a few dozen points per continent;
    // real 1:110m coastline has hundreds. Both render — only one is recognisable.
    expect(RINGS.length).toBeGreaterThan(50);
    const biggest = Math.max(...RINGS.map((r) => r.length));
    expect(biggest).toBeGreaterThan(400);
    // Every ring is a closed area, not a stray line.
    for (const ring of RINGS) expect(ring.length).toBeGreaterThanOrEqual(4);
  });

  it('stays small enough to bundle', () => {
    // It ships in the app bundle, so size is a real constraint — the whole reason it is simplified
    // rather than shipped at full resolution. A regeneration that forgot to simplify lands here.
    const bytes = WORLD_OUTLINE.reduce((n, p) => n + p.length, 0);
    expect(bytes).toBeLessThan(120_000);
  });
});
