// SPDX-License-Identifier: AGPL-3.0-only
//
// The coastline is generated data, so what is worth testing is not the numbers but the claim they
// make: **is there land where land is, and water where water is**. The first version of this file's
// subject was hand-traced and drew continents as blobs — visibly wrong to anyone who looked, and
// caught by a human rather than by anything here. This is what would have caught it.
//
// The second version was real data at 1:110m, and the complaint that retired it was the opposite
// one: correct at the world view, and a polygon of a dozen straight edges once Japan filled the
// pane. So the checks below also pin the *resolution* — coastal detail the coarse set could not
// hold — and the inland water the coarse set drew as land.

import { describe, expect, it } from 'vitest';
import { MAP_HEIGHT, MAP_WIDTH, project } from './geoProjection';
import { WORLD_LAKES, WORLD_OUTLINE } from './worldOutline';

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

const LAND = WORLD_OUTLINE.map(ringPoints);
const WATER = WORLD_LAKES.map(ringPoints);

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

/** Whether the map says this coordinate is on land: inside a landmass and not inside a lake drawn
 *  over it — the same order the page paints in. */
function isLand(lat: number, lon: number): boolean {
  const p = project(lat, lon);
  const pt: [number, number] = [p.x, p.y];
  return LAND.some((r) => inRing(pt, r)) && !WATER.some((r) => inRing(pt, r));
}

describe('WORLD_OUTLINE', () => {
  it('puts land under cities and water under oceans', () => {
    // Spread deliberately across every continent and every quadrant of the map, because the failure
    // modes are regional: a sign error flips a hemisphere, an offset shifts one continent, and a
    // bad simplification eats one landmass while leaving the rest correct.
    const cities: [string, number, number][] = [
      ['Tokyo', 35.68, 139.69],
      ['London', 51.51, -0.13],
      // Midtown, not the Battery: at 1:10m the Hudson is drawn, and its ≈3 km simplification
      // reaches the tip of Manhattan. A test point has to sit further from a coast than the
      // tolerance, or it tests the rounding rather than the map.
      ['New York', 40.75, -73.98],
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

  it('holds coastal detail the 1:110m set could not', () => {
    // Every point here was measured against the retired 1:110m outline and came back wrong there:
    // an island it did not have, a bay it filled in. That is what an operator zoomed on their own
    // country sees at once, and a regeneration from the coarse set — or at a tolerance wide enough
    // to erase these — fails here rather than in a screenshot. (Points the coarse set already got
    // right, like Shikoku or the Isle of Wight, are deliberately not listed: they would pass on
    // either outline and say nothing about resolution.)
    const land: [string, number, number][] = [
      ['Awaji island', 34.38, 134.85],
      ['Sado island', 38.0, 138.4],
      ['Long Island, Queens', 40.72, -73.82],
      ['Long Island, Montauk', 41.04, -71.94],
    ];
    for (const [name, lat, lon] of land) {
      expect(isLand(lat, lon), `${name} should be on land`).toBe(true);
    }
    const water: [string, number, number][] = [
      ['Tokyo Bay', 35.45, 139.85],
      ['Ise Bay', 34.8, 136.75],
    ];
    for (const [name, lat, lon] of water) {
      expect(isLand(lat, lon), `${name} should be water`).toBe(false);
    }
  });

  it('draws the inland seas and great lakes as water', () => {
    // The coarse outline painted all of these as land: Natural Earth cuts the Caspian out of
    // Eurasia as a polygon hole, which a one-ring-per-string renderer fills in, and the lakes are
    // a separate layer it never had. `WORLD_LAKES` is drawn over the land in the ocean colour.
    const inland: [string, number, number][] = [
      ['Caspian Sea', 42, 50.5],
      ['Lake Superior', 47.7, -87.5],
      ['Lake Michigan', 43.5, -87.0],
      ['Lake Baikal', 53.5, 108.0],
      ['Lake Victoria', -1.5, 33.0],
      ['Lake Biwa', 35.25, 136.08],
    ];
    for (const [name, lat, lon] of inland) {
      expect(isLand(lat, lon), `${name} should be water`).toBe(false);
    }
    // And the shore beside each is still land — a lake ring that swallowed its city is the other
    // failure a fill-over-land scheme can have.
    expect(isLand(45.0, 51.0), 'Atyrau, on the Caspian shore').toBe(true);
    expect(isLand(41.85, -87.75), 'Chicago, west of the Lake Michigan shore').toBe(true);
    expect(isLand(35.0, 135.87), 'Ōtsu, on the Lake Biwa shore').toBe(true);
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
    // One assertion over the lot rather than five per point: there are ~70,000 points now, and
    // 350,000 `expect` calls is slow enough to time out under a parallel full-suite run.
    const stray: [number, number][] = [];
    for (const ring of [...LAND, ...WATER]) {
      for (const [x, y] of ring) {
        if (
          !Number.isFinite(x) ||
          !Number.isFinite(y) ||
          x < 0 ||
          x > MAP_WIDTH ||
          y < 0 ||
          y > MAP_HEIGHT
        ) {
          stray.push([x, y]);
        }
      }
    }
    expect(stray).toEqual([]);
  });

  it('is real geometry rather than a handful of blobs', () => {
    // The regression this exists for. A hand-traced outline has a few dozen points per continent;
    // the 1:110m set had ~1,500 on its biggest ring and the 1:10m set has over 10,000. All three
    // render — only the last is recognisable with a prefecture filling the pane.
    expect(LAND.length).toBeGreaterThan(300);
    const biggest = Math.max(...LAND.map((r) => r.length));
    expect(biggest).toBeGreaterThan(8_000);
    // Every ring is a closed area, not a stray line.
    for (const ring of [...LAND, ...WATER]) expect(ring.length).toBeGreaterThanOrEqual(3);
  });

  it('stays small enough to bundle', () => {
    // It ships in the app bundle (lazily, with the Topology route group), so size is a real
    // constraint — the whole reason it is simplified rather than shipped at full resolution. The
    // 1:10m set at a 0.06-unit tolerance and one decimal is ~825 KB of path data, ~235 KB gzipped;
    // a regeneration that forgot to simplify, or kept two decimals, lands here.
    const bytes = [...WORLD_OUTLINE, ...WORLD_LAKES].reduce((n, p) => n + p.length, 0);
    expect(bytes).toBeLessThan(900_000);
  });
});
