// SPDX-License-Identifier: AGPL-3.0-only
//
// The fine coastline tiles (ADR-207) are generated data, so the claims worth pinning are the ones
// a bad regeneration would break: each file stays inside its square, the set stays small enough to
// ship, and where the world outline gives the wrong answer at a high zoom the tiles give the right
// one — which is the reason they exist.

import { describe, expect, it } from 'vitest';
import { GEO_TILE_UNITS, MAP_HEIGHT, MAP_WIDTH, project } from './geoProjection';
import {
  type GeoTile,
  hasGeoTile,
  placeGeoTiles,
  tileKeyOf,
  tileOrigin,
  wantedTiles,
} from './geoTiles';
import { WORLD_LAKES, WORLD_OUTLINE } from './worldOutline';

const FILES = import.meta.glob<GeoTile>('./geoTileData/*.json', { eager: true, import: 'default' });
const TILES = new Map(Object.entries(FILES).map(([p, t]) => [tileKeyOf(p), t]));

type Pt = [number, number];

/** A tile path (`M x y l dx dy dx dy … [z]`, integer thousandths) back into points. */
function tilePoints(d: string): Pt[] {
  const n = d.replace(/[Mlz]/g, ' ').trim().split(/\s+/).map(Number);
  const pts: Pt[] = [];
  let x = 0;
  let y = 0;
  for (let i = 0; i < n.length; i += 2) {
    x = i === 0 ? n[0] : x + n[i];
    y = i === 0 ? n[1] : y + n[i + 1];
    pts.push([x, y]);
  }
  return pts;
}

/** The world outline's `M…L…Z` rings, in map units. */
function outlinePoints(path: string): Pt[] {
  return path
    .slice(1, -1)
    .split(/[ML]/)
    .filter((s) => s.length > 0)
    .map((pair) => pair.trim().split(' ').map(Number) as Pt);
}

function inRing([px, py]: Pt, ring: Pt[]): boolean {
  let inside = false;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    const [xi, yi] = ring[i];
    const [xj, yj] = ring[j];
    if (yi > py !== yj > py && px < ((xj - xi) * (py - yi)) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}

const COARSE_LAND = WORLD_OUTLINE.map(outlinePoints);
const COARSE_WATER = WORLD_LAKES.map(outlinePoints);

function coarseIsLand(lat: number, lon: number): boolean {
  const p = project(lat, lon);
  const pt: Pt = [p.x, p.y];
  return COARSE_LAND.some((r) => inRing(pt, r)) && !COARSE_WATER.some((r) => inRing(pt, r));
}

/** What the tiles say, painted the way the page paints them: land, then water over it. */
function fineIsLand(lat: number, lon: number): boolean {
  const p = project(lat, lon);
  const c = Math.floor(p.x / GEO_TILE_UNITS);
  const r = Math.floor(p.y / GEO_TILE_UNITS);
  const tile = TILES.get(`${c}_${r}`);
  if (!tile) return false;
  const pt: Pt = [(p.x - c * GEO_TILE_UNITS) * 1000, (p.y - r * GEO_TILE_UNITS) * 1000];
  return (
    tile.land.some((d) => inRing(pt, tilePoints(d))) &&
    !tile.water.some((d) => inRing(pt, tilePoints(d)))
  );
}

describe('the fine coastline tiles', () => {
  it('names every file after a square of the grid', () => {
    const cols = MAP_WIDTH / GEO_TILE_UNITS;
    const rows = MAP_HEIGHT / GEO_TILE_UNITS;
    // A regeneration that wrote nothing, or wrote ocean squares too, lands outside this range.
    expect(TILES.size).toBeGreaterThan(400);
    expect(TILES.size).toBeLessThan(cols * rows);
    for (const key of TILES.keys()) {
      const { x, y } = tileOrigin(key);
      expect(x >= 0 && x < MAP_WIDTH && y >= 0 && y < MAP_HEIGHT, key).toBe(true);
    }
  });

  it('keeps each file inside its square', () => {
    // Fills may reach the overlap past the edge (0.01 units = 10 here); a shoreline may not pass
    // the edge at all. A tile cut at another size than `GEO_TILE_DEG` fails this.
    const side = GEO_TILE_UNITS * 1000;
    const out: string[] = [];
    for (const [key, t] of TILES) {
      const check = (paths: string[], slack: number) => {
        for (const d of paths) {
          for (const [x, y] of tilePoints(d)) {
            if (x < -slack || y < -slack || x > side + slack || y > side + slack) {
              out.push(`${key} ${x},${y}`);
              return;
            }
          }
        }
      };
      check(t.land, 11);
      check(t.water, 11);
      check(t.coast, 1);
    }
    expect(out).toEqual([]);
  });

  it('stays small enough to ship', () => {
    // Measured at generation (≈110 m, ADR-207): 7.2 MB over 465 files, the heaviest 186 KB. Only
    // the visible ones load, so the per-file ceiling is the one an operator waits on.
    let total = 0;
    let heaviest = 0;
    for (const t of TILES.values()) {
      const bytes = JSON.stringify(t).length;
      total += bytes;
      heaviest = Math.max(heaviest, bytes);
    }
    expect(total).toBeLessThan(8_000_000);
    expect(heaviest).toBeLessThan(220_000);
  });

  it('answers right where the world outline is too coarse', () => {
    // Points where the source data and the world outline disagree, found by sampling both and
    // keeping those whose answer holds 1.5 km around. Each first half asserts the outline gets it
    // wrong — so the point still tests resolution — and the second that the tiles get it right.
    const probes: [string, number, number, boolean][] = [
      ['Matsushima Bay', 38.335, 141.08, false],
      ['Futtsu, east shore of Tokyo Bay', 35.34, 139.86, true],
      ['an island in the Seto Inland Sea', 34.16, 133.06, true],
      ['a fjord south of Bergen', 60.06, 5.16, false],
    ];
    for (const [name, lat, lon, land] of probes) {
      expect(coarseIsLand(lat, lon), `${name}: outline`).toBe(!land);
      expect(fineIsLand(lat, lon), `${name}: tiles`).toBe(land);
    }
  });

  it('agrees with the world outline away from the coast', () => {
    const points: [string, number, number, boolean][] = [
      ['Tokyo', 35.68, 139.69, true],
      ['Denver', 39.74, -104.99, true],
      ['Lake Baikal', 53.5, 108.0, false],
      ['the Caspian Sea', 42.0, 50.5, false],
      ['the Sea of Japan', 40.0, 135.0, false],
    ];
    for (const [name, lat, lon, land] of points) {
      expect(fineIsLand(lat, lon), name).toBe(land);
      expect(coarseIsLand(lat, lon), name).toBe(land);
    }
  });

  it('asks only for squares that have a file', () => {
    // Japan's square has land; one of the open South Pacific has none and was never written.
    const japan = '31_5';
    const pacific = '3_12';
    expect(hasGeoTile(japan)).toBe(true);
    expect(hasGeoTile(pacific)).toBe(false);
    expect(wantedTiles([pacific, japan, '30_5'])).toEqual(['30_5', japan]);
  });
});

describe('placeGeoTiles', () => {
  it('puts each tile at its square, scaled from path units to map units', () => {
    const tile: GeoTile = { land: [], water: [], coast: [] };
    const [p] = placeGeoTiles([['3_2', tile]]);
    expect(p.o).toEqual({ x: 3 * GEO_TILE_UNITS, y: 2 * GEO_TILE_UNITS });
    expect(p.at).toBe(`translate(${3 * GEO_TILE_UNITS} ${2 * GEO_TILE_UNITS}) scale(0.001)`);
    expect(p.tile).toBe(tile);
  });
});
