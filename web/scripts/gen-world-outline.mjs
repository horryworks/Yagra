#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-only
// Regenerates `src/pages/worldOutline.ts` from Natural Earth GeoJSON.
//
// **Author-time only.** Not part of the build, not run in CI, and the app never reads the source
// data: the output is committed, so the WebUI keeps working on a management network with no egress
// (see the header of the generated file for why that matters).
//
// Source data (public domain, no rights reserved):
//   https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_50m_land.geojson
//   https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_50m_lakes.geojson
// (`ne_10m_*` is the finer set, `ne_110m_*` the coarse one the first generation used.)
//
// Usage:
//   node scripts/gen-world-outline.mjs --land ne_50m_land.geojson --lakes ne_50m_lakes.geojson
//       [--tol 0.06] [--min-area 0.01] [--lake-min-area 0.2] [--decimals 1]
//       [--out src/pages/worldOutline.ts] [--dry]
//
// The pipeline is: reproject every ring into the 720 × 360 equirectangular grid `geoProjection.ts`
// draws in (2 units per degree) → Ramer–Douglas–Peucker at `--tol` units → drop rings whose area is
// below `--min-area` units² (sub-pixel specks) → sort largest-first so continents paint before the
// islands sitting on them → emit one SVG path per ring. `--dry` prints the statistics and writes
// nothing.

import { readFileSync, writeFileSync } from 'node:fs';
import { basename } from 'node:path';

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
