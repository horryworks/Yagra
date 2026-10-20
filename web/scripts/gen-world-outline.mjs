#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-only
// Regenerates `src/pages/worldOutline.ts` from Natural Earth GeoJSON.
//
// **Author-time only.** Not part of the build, not run in CI, and the app never reads the source
// data: the output is committed, so the WebUI keeps working on a management network with no egress
// (see the header of the generated file for why that matters).
//
// Source data (public domain, no rights reserved):
//   https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_10m_land.geojson
//   https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_10m_lakes.geojson
// (`ne_10m_*` is what both outputs use; `ne_50m_*` and `ne_110m_*` are the coarser sets.)
//
// Usage:
//   node scripts/gen-world-outline.mjs --land ne_50m_land.geojson --lakes ne_50m_lakes.geojson
//       [--tol 0.06] [--min-area 0.01] [--lake-min-area 0.2] [--decimals 1]
//       [--out src/pages/worldOutline.ts] [--dry]
//   node scripts/gen-world-outline.mjs --land ne_10m_land.geojson --lakes ne_10m_lakes.geojson \
//       --tiles src/pages/geoTileData --tol 0.004 --min-area 0.0002 --lake-min-area 0.0005 [--dry]
//
// The pipeline is: reproject every ring into the 720 × 360 equirectangular grid `geoProjection.ts`
// draws in (2 units per degree) → Ramer–Douglas–Peucker at `--tol` units → drop rings whose area is
// below `--min-area` units² (sub-pixel specks) → sort largest-first so continents paint before the
// islands sitting on them → emit one SVG path per ring. `--dry` prints the statistics and writes
// nothing.
//
// `--tiles <dir>` writes the fine set instead (ADR-207): the same pipeline at a much smaller
// tolerance, cut into `TILE_DEG`° squares, one JSON file per square that holds any land or water.
// Each file holds three lists of path data in integer thousandths of a map unit, relative to the
// square's corner: `land` and `water` are filled shapes clipped a little beyond the square (so two
// neighbours overlap instead of leaving a hairline seam) and are drawn without a stroke, and
// `coast` is the shoreline clipped exactly to the square as open lines — the only thing stroked,
// so a square's edge never reads as a coast.

import { mkdirSync, readdirSync, readFileSync, unlinkSync, writeFileSync } from 'node:fs';
import { basename, join } from 'node:path';

const MAP_WIDTH = 720;
const MAP_HEIGHT = 360;

// ---- arguments ---------------------------------------------------------------------------------

const args = process.argv.slice(2);
function opt(name, fallback) {
  const i = args.indexOf(`--${name}`);
  if (i < 0) return fallback;
  const v = args[i + 1];
  if (v === undefined || v.startsWith('--')) return true;
  return v;
}
const LAND = opt('land', null);
const LAKES = opt('lakes', null);
const TOL = Number(opt('tol', 0.06));
const MIN_AREA = Number(opt('min-area', 0.01));
const LAKE_MIN_AREA = Number(opt('lake-min-area', 0.2));
const DECIMALS = Number(opt('decimals', 1));
const OUT = opt('out', 'src/pages/worldOutline.ts');
const DRY = opt('dry', false) === true;
const TILES = opt('tiles', null);
if (!LAND || LAND === true) {
  console.error(
    'usage: gen-world-outline.mjs --land <ne_XXm_land.geojson> [--lakes <ne_XXm_lakes.geojson>] …',
  );
  process.exit(2);
}

// ---- geometry ----------------------------------------------------------------------------------

/** WGS-84 degrees → map units. Same formula as `geoProjection.ts::project`, clamped the same way. */
function project([lon, lat]) {
  const x = ((Math.max(-180, Math.min(180, lon)) + 180) / 360) * MAP_WIDTH;
  const y = ((90 - Math.max(-90, Math.min(90, lat))) / 180) * MAP_HEIGHT;
  return [x, y];
}

/** Perpendicular distance from `p` to the segment `a`–`b`. */
function segDist(p, a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const len2 = dx * dx + dy * dy;
  let t = len2 === 0 ? 0 : ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2;
  t = Math.max(0, Math.min(1, t));
  const ex = a[0] + t * dx - p[0];
  const ey = a[1] + t * dy - p[1];
  return Math.sqrt(ex * ex + ey * ey);
}

/** Ramer–Douglas–Peucker, iterative (Antarctica at 1:10m is long enough to worry about a stack). */
function rdp(points, tol) {
  const n = points.length;
  if (n <= 2) return points.slice();
  const keep = new Uint8Array(n);
  keep[0] = 1;
  keep[n - 1] = 1;
  const stack = [[0, n - 1]];
  while (stack.length > 0) {
    const [s, e] = stack.pop();
    let maxD = -1;
    let maxI = -1;
    for (let i = s + 1; i < e; i++) {
      const d = segDist(points[i], points[s], points[e]);
      if (d > maxD) {
        maxD = d;
        maxI = i;
      }
    }
    if (maxD > tol) {
      keep[maxI] = 1;
      stack.push([s, maxI], [maxI, e]);
    }
  }
  const out = [];
  for (let i = 0; i < n; i++) if (keep[i]) out.push(points[i]);
  return out;
}

/** Shoelace area, absolute, in map units². */
function area(ring) {
  let a = 0;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    a += (ring[j][0] + ring[i][0]) * (ring[j][1] - ring[i][1]);
  }
  return Math.abs(a / 2);
}

function fmt(n) {
  let s = n.toFixed(DECIMALS);
  if (s.includes('.')) s = s.replace(/0+$/, '').replace(/\.$/, '');
  return s === '-0' ? '0' : s;
}

/** One closed ring → `M…L…Z`, with the GeoJSON closing point dropped and rounded duplicates merged. */
function toPath(ring) {
  const pts = [];
  let last = null;
  for (const p of ring) {
    const q = `${fmt(p[0])} ${fmt(p[1])}`;
    if (q !== last) pts.push(q);
    last = q;
  }
  if (pts.length > 1 && pts[0] === pts[pts.length - 1]) pts.pop();
  if (pts.length < 3) return null;
  return `M${pts.join('L')}Z`;
}

/** Every ring of every polygon in a GeoJSON FeatureCollection, as `{ outer, ring, name }`. */
function* rings(geojson) {
  for (const f of geojson.features) {
    const g = f.geometry;
    if (!g) continue;
    const polys =
      g.type === 'Polygon' ? [g.coordinates] : g.type === 'MultiPolygon' ? g.coordinates : [];
    for (const poly of polys) {
      for (let i = 0; i < poly.length; i++) {
        yield { outer: i === 0, ring: poly[i], name: f.properties?.name };
      }
    }
  }
}

/** Project + simplify + area-filter one dataset. Outer rings come back largest-first; holes apart. */
function prepare(geojson, minArea) {
  const outers = [];
  const holes = [];
  for (const { outer, ring, name } of rings(geojson)) {
    const projected = rdp(ring.map(project), TOL);
    const a = area(projected);
    if (!outer) {
      holes.push({ ring: projected, area: a, name });
      continue;
    }
    if (a < minArea) continue;
    outers.push({ ring: projected, area: a, name });
  }
  outers.sort((p, q) => q.area - p.area);
  return { outers, holes };
}

// ---- tiling (ADR-207) --------------------------------------------------------------------------

/** Degrees per tile side. Must match `GEO_TILE_DEG` in `src/pages/geoProjection.ts`. */
const TILE_DEG = 10;
const TILE_UNITS = (TILE_DEG * MAP_WIDTH) / 360;
const TILE_COLS = MAP_WIDTH / TILE_UNITS;
const TILE_ROWS = MAP_HEIGHT / TILE_UNITS;
/** How far a filled shape is kept beyond its square, in map units (≈2 px at the 240× ceiling). */
const TILE_OVERLAP = 0.01;
/** Path data resolution: thousandths of a map unit (≈55 m, a quarter pixel at the ceiling). */
const TILE_SCALE = 1000;

/** Sutherland–Hodgman against one half-plane: keep the points where `inside(p)`. */
function clipEdge(ring, inside, cross) {
  const out = [];
  for (let i = 0; i < ring.length; i++) {
    const cur = ring[i];
    const prev = ring[(i + ring.length - 1) % ring.length];
    const ci = inside(cur);
    const pi = inside(prev);
    if (ci) {
      if (!pi) out.push(cross(prev, cur));
      out.push(cur);
    } else if (pi) {
      out.push(cross(prev, cur));
    }
  }
  return out;
}

/** A closed ring clipped to the box `[x0,x1]×[y0,y1]`; empty when nothing is left. */
function clipRing(ring, x0, y0, x1, y1) {
  const atX = (x) => (a, b) => [x, a[1] + ((b[1] - a[1]) * (x - a[0])) / (b[0] - a[0])];
  const atY = (y) => (a, b) => [a[0] + ((b[0] - a[0]) * (y - a[1])) / (b[1] - a[1]), y];
  let r = ring;
  r = clipEdge(r, (p) => p[0] >= x0, atX(x0));
  if (r.length) r = clipEdge(r, (p) => p[0] <= x1, atX(x1));
  if (r.length) r = clipEdge(r, (p) => p[1] >= y0, atY(y0));
  if (r.length) r = clipEdge(r, (p) => p[1] <= y1, atY(y1));
  return r.length >= 3 ? r : [];
}

/** Liang–Barsky: the part of segment a→b inside the box, or null. */
function clipSegment(a, b, x0, y0, x1, y1) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  let t0 = 0;
  let t1 = 1;
  for (const [p, q] of [
    [-dx, a[0] - x0],
    [dx, x1 - a[0]],
    [-dy, a[1] - y0],
    [dy, y1 - a[1]],
  ]) {
    if (p === 0) {
      if (q < 0) return null;
      continue;
    }
    const t = q / p;
    if (p < 0) {
      if (t > t1) return null;
      if (t > t0) t0 = t;
    } else {
      if (t < t0) return null;
      if (t < t1) t1 = t;
    }
  }
  return [
    [a[0] + t0 * dx, a[1] + t0 * dy],
    [a[0] + t1 * dx, a[1] + t1 * dy],
  ];
}

/** A closed ring's shoreline inside the box, as open polylines. */
function clipLines(ring, x0, y0, x1, y1) {
  const lines = [];
  let cur = null;
  for (let i = 0; i < ring.length; i++) {
    const seg = clipSegment(ring[i], ring[(i + 1) % ring.length], x0, y0, x1, y1);
    if (!seg) {
      cur = null;
      continue;
    }
    const last = cur && cur[cur.length - 1];
    if (last && last[0] === seg[0][0] && last[1] === seg[0][1]) {
      cur.push(seg[1]);
    } else {
      cur = [seg[0], seg[1]];
      lines.push(cur);
    }
  }
  return lines;
}

/** Points → a relative SVG path in integer thousandths of a unit from the tile corner. */
function tilePath(points, ox, oy, closed) {
  let d = '';
  let px = 0;
  let py = 0;
  let n = 0;
  for (const p of points) {
    const x = Math.round((p[0] - ox) * TILE_SCALE);
    const y = Math.round((p[1] - oy) * TILE_SCALE);
    if (n > 0 && x === px && y === py) continue;
    if (n === 0) d += `M${x} ${y}`;
    else d += `${n === 1 ? 'l' : ' '}${x - px} ${y - py}`;
    px = x;
    py = y;
    n++;
  }
  if (n < (closed ? 3 : 2)) return null;
  return closed ? `${d}z` : d;
}

/** The bounding box of a ring, for skipping the tiles it cannot touch. */
function bbox(ring) {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const [x, y] of ring) {
    if (x < x0) x0 = x;
    if (x > x1) x1 = x;
    if (y < y0) y0 = y;
    if (y > y1) y1 = y;
  }
  return { x0, y0, x1, y1 };
}

function writeTiles(dir, landRings, waterRings) {
  const tiles = new Map();
  const tileOf = (c, r) => {
    const k = `${c}_${r}`;
    if (!tiles.has(k)) tiles.set(k, { land: [], water: [], coast: [] });
    return tiles.get(k);
  };
  const add = (rings, kind) => {
    for (const ring of rings) {
      const b = bbox(ring);
      const c0 = Math.max(0, Math.floor((b.x0 - TILE_OVERLAP) / TILE_UNITS));
      const c1 = Math.min(TILE_COLS - 1, Math.floor((b.x1 + TILE_OVERLAP) / TILE_UNITS));
      const r0 = Math.max(0, Math.floor((b.y0 - TILE_OVERLAP) / TILE_UNITS));
      const r1 = Math.min(TILE_ROWS - 1, Math.floor((b.y1 + TILE_OVERLAP) / TILE_UNITS));
      for (let c = c0; c <= c1; c++) {
        for (let r = r0; r <= r1; r++) {
          const x0 = c * TILE_UNITS;
          const y0 = r * TILE_UNITS;
          const x1 = x0 + TILE_UNITS;
          const y1 = y0 + TILE_UNITS;
          const o = TILE_OVERLAP;
          const fill = clipRing(ring, x0 - o, y0 - o, x1 + o, y1 + o);
          if (fill.length === 0) continue;
          const t = tileOf(c, r);
          const d = tilePath(fill, x0, y0, true);
          if (d) t[kind].push(d);
          for (const line of clipLines(ring, x0, y0, x1, y1)) {
            const l = tilePath(line, x0, y0, false);
            if (l) t.coast.push(l);
          }
        }
      }
    }
  };
  add(landRings, 'land');
  add(waterRings, 'water');

  let total = 0;
  let heaviest = { key: '', bytes: 0 };
  const out = [];
  for (const [key, t] of [...tiles].sort(([a], [b]) => a.localeCompare(b))) {
    if (t.land.length === 0 && t.water.length === 0) continue;
    const json = `${JSON.stringify(t)}\n`;
    total += json.length;
    if (json.length > heaviest.bytes) heaviest = { key, bytes: json.length };
    out.push([key, json]);
  }
  console.error(
    `tiles: ${out.length} files, ${total} bytes, heaviest t${heaviest.key} ${heaviest.bytes} bytes`,
  );
  if (DRY) return;
  mkdirSync(dir, { recursive: true });
  for (const f of readdirSync(dir)) if (/^t\d+_\d+\.json$/.test(f)) unlinkSync(join(dir, f));
  for (const [key, json] of out) writeFileSync(join(dir, `t${key}.json`), json);
  console.error(`wrote ${out.length} tiles to ${dir}`);
}

// ---- run ---------------------------------------------------------------------------------------

const land = JSON.parse(readFileSync(LAND, 'utf8'));
const landPrepared = prepare(land, MIN_AREA);
const landPaths = landPrepared.outers.map((r) => toPath(r.ring)).filter(Boolean);

// Water that sits on land: the holes in the land polygons (Natural Earth cuts the Caspian out of
// Eurasia as a hole) plus the lakes layer, both drawn in the ocean colour on top of the land.
const water = landPrepared.holes.filter((h) => h.area >= LAKE_MIN_AREA);
let lakeCount = 0;
if (LAKES && LAKES !== true) {
  const lakes = JSON.parse(readFileSync(LAKES, 'utf8'));
  const lp = prepare(lakes, LAKE_MIN_AREA);
  lakeCount = lp.outers.length;
  water.push(...lp.outers);
}
water.sort((p, q) => q.area - p.area);
const lakePaths = water.map((r) => toPath(r.ring)).filter(Boolean);

const landPoints = landPrepared.outers.reduce((n, r) => n + r.ring.length, 0);
const landBytes = landPaths.reduce((n, p) => n + p.length, 0);
const lakeBytes = lakePaths.reduce((n, p) => n + p.length, 0);
const biggest = Math.max(...landPrepared.outers.map((r) => r.ring.length));
console.error(
  `land: ${landPaths.length} rings, ${landPoints} points, biggest ring ${biggest} points, ${landBytes} bytes` +
    ` | water-on-land: ${lakePaths.length} rings (${landPrepared.holes.length} holes + ${lakeCount} lakes), ${lakeBytes} bytes` +
    ` | tol ${TOL} min-area ${MIN_AREA} lake-min-area ${LAKE_MIN_AREA} decimals ${DECIMALS}`,
);
// ---- fine tiles (ADR-207) ----------------------------------------------------------------------

if (TILES && TILES !== true) {
  writeTiles(
    TILES,
    landPrepared.outers.map((r) => r.ring),
    water.map((r) => r.ring),
  );
  process.exit(0);
}

if (DRY) process.exit(0);

const kmPerUnit = 55.6; // 0.5° of longitude at the equator
const header = `// SPDX-License-Identifier: AGPL-3.0-only
// The world coastline as SVG path data, in the same equirectangular grid \`geoProjection.ts\`
// projects into (720 × 360 units, 2 units per degree, 0,0 = 180°W 90°N).
//
// **Generated, not drawn.** \`scripts/gen-world-outline.mjs\` produced this file from Natural Earth
// \`${basename(LAND)}\`${LAKES && LAKES !== true ? ` and \`${basename(LAKES)}\`` : ''} — public domain, no rights reserved
// (naturalearthdata.com / github.com/nvkelso/natural-earth-vector) — reprojected and simplified
// with Ramer–Douglas–Peucker at a ${TOL}-unit tolerance (≈${(TOL * kmPerUnit).toFixed(1)} km at the
// equator, under one screen pixel at the maximum zoom). Land rings below ${MIN_AREA} units² are
// dropped as sub-pixel specks; water rings below ${LAKE_MIN_AREA} units² likewise. Both lists are
// sorted largest-first so continents paint before islands.
//
// The first attempt at this file was traced by hand and was, accurately, described as 適当すぎる —
// the continents were blobs. Hand-drawing a coastline is the kind of task that looks approximately
// right to the person doing it and obviously wrong to everyone else, which is the definition of a
// job for real data. The second was generated from the 1:110m set at a 0.35-unit tolerance, which
// was right at the world view and visibly polygonal at any zoom an operator uses to find a site.
//
// The conversion is **author-time** and its output is committed, so the app has no build step, no
// runtime fetch, and no dependency on the source data or on a projection library. To regenerate,
// fetch the GeoJSON files named in the script and run it; this constant is the artefact.
//
// Bundled rather than pulled from a tile server, because a monitoring console is the thing you
// open when the network is broken — and Yagra is routinely deployed on isolated management
// networks with no egress at all. A map that needs the internet is blank exactly when it matters.
//
// Accuracy is a locator map's: good enough to see which coast a site is on, not a navigation
// chart. \`geoProjection.test.ts\` pins the projection this is drawn in against known coordinates,
// and \`worldOutline.test.ts\` checks these rings actually put land where land is (and water where
// the Caspian is).

/**
 * Landmass outlines as SVG path data, largest first.
 *
 * One string per ring so a renderer can style them uniformly and the browser can cull cheaply.
 * Coordinates are in map units; \`MAP_WIDTH\`/\`MAP_HEIGHT\` in \`geoProjection.ts\` define the space.
 */
export const WORLD_OUTLINE: readonly string[] = [
`;

const lakesDoc = `
/**
 * Water that sits inside a landmass — the Caspian Sea (a hole in Natural Earth's Eurasia polygon),
 * the Great Lakes, Baikal, Victoria and the rest above the area floor — largest first.
 *
 * Drawn **after** \`WORLD_OUTLINE\` in the ocean colour rather than cut out as path holes: the land
 * list stays one plain ring per string, and a renderer that ignores this list still draws a
 * correct-enough map with the lakes filled in.
 */
export const WORLD_LAKES: readonly string[] = [
`;

const body =
  header +
  landPaths.map((p) => `  ${JSON.stringify(p)},\n`).join('') +
  '];\n' +
  lakesDoc +
  lakePaths.map((p) => `  ${JSON.stringify(p)},\n`).join('') +
  '];\n';
writeFileSync(OUT, body);
console.error(`wrote ${OUT} (${body.length} bytes)`);
