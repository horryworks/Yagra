#!/usr/bin/env node
// SPDX-License-Identifier: AGPL-3.0-only
// Regenerates `src/pages/geoRelief.json`, the elevation and depth bands Geo map draws when its
// relief switch is on (ADR-220), from NOAA's ETOPO 2022 grid.
//
// **Author-time only**, like `gen-world-outline.mjs`: not part of the build, not run in CI, and
// the app never reads the source data. The output is committed.
//
// Source data (public domain, a work of the U.S. Government):
//   https://www.ngdc.noaa.gov/mgg/global/relief/ETOPO2022/data/60s/60s_surface_elev_gtif/ETOPO_2022_v1_60s_N90W180_surface.tif
// (466 MB: 21600 x 10800 float32 metres, 1 arc-minute cells, origin 180°W 90°N, deflate with the
// floating-point predictor in 256 x 256 tiles. The reader below handles exactly that layout and
// refuses anything else.)
//
// Usage:
//   node scripts/gen-geo-relief.mjs --tif ETOPO_2022_v1_60s_N90W180_surface.tif
//       [--block 6] [--tol 0.06] [--min-area 0.02] [--out src/pages/geoRelief.json] [--dry]
//
// The pipeline is: average `--block` x `--block` source cells (6 = 0.1°) → for each level,
// marching squares over { elevation >= level } on a grid padded with "outside", so every ring
// closes → Ramer–Douglas–Peucker at `--tol` map units → drop rings below `--min-area` units² →
// one evenodd SVG path per level, in the 720 x 360 grid `geoProjection.ts` draws in. Levels are
// superlevel sets of one field, so they nest, and the page paints them lowest first.

import { openSync, readSync, writeFileSync } from 'node:fs';
import { gzipSync, inflateSync } from 'node:zlib';

const MAP_WIDTH = 720;
const MAP_HEIGHT = 360;
/** How far a band runs past 180° on each side, in map units (the smallest step the output keeps). */
const EW_OVERLAP = 0.1;

/** The levels, in metres. Must match `RELIEF_SEA_LEVELS` / `RELIEF_LAND_LEVELS` in
 *  `src/pages/geoRelief.ts`, which a test compares with this file's output. */
const SEA_LEVELS = [-5000, -3000, -1000, -200];
const LAND_LEVELS = [200, 500, 1000, 2000, 3000, 4500];

// ---- arguments ---------------------------------------------------------------------------------

const args = process.argv.slice(2);
function opt(name, fallback) {
  const i = args.indexOf(`--${name}`);
  if (i < 0) return fallback;
  const v = args[i + 1];
  if (v === undefined || v.startsWith('--')) return true;
  return v;
}
const TIF = opt('tif', null);
const BLOCK = Number(opt('block', 6));
const TOL = Number(opt('tol', 0.06));
const MIN_AREA = Number(opt('min-area', 0.02));
const OUT = opt('out', 'src/pages/geoRelief.json');
const DRY = opt('dry', false) === true;
if (!TIF || TIF === true) {
  console.error('usage: gen-geo-relief.mjs --tif <ETOPO_2022_v1_60s_N90W180_surface.tif> …');
  process.exit(2);
}

// ---- the GeoTIFF -------------------------------------------------------------------------------

const SRC_W = 21600;
const SRC_H = 10800;
const TILE = 256;

/** Read the source and return the mean over `BLOCK` x `BLOCK` cells, row 0 = north. */
function readGrid() {
  const fd = openSync(TIF, 'r');
  const read = (off, len) => {
    const b = Buffer.alloc(len);
    readSync(fd, b, 0, len, off);
    return b;
  };
  const head = read(0, 8);
  if (head.toString('latin1', 0, 2) !== 'II' || head.readUInt16LE(2) !== 42) {
    throw new Error('not a little-endian classic TIFF');
  }
  const ifd = head.readUInt32LE(4);
  const n = read(ifd, 2).readUInt16LE(0);
  const ent = read(ifd + 2, n * 12);
  const tags = new Map();
  for (let i = 0; i < n; i++) {
    tags.set(ent.readUInt16LE(i * 12), {
      count: ent.readUInt32LE(i * 12 + 4),
      // A SHORT that fits is stored in the low half of the value field.
      value: ent.readUInt16LE(i * 12 + 2) === 3 ? ent.readUInt16LE(i * 12 + 8) : ent.readUInt32LE(i * 12 + 8),
    });
  }
  const expect = { 256: SRC_W, 257: SRC_H, 258: 32, 259: 8, 317: 3, 322: TILE, 323: TILE, 339: 3 };
  for (const [tag, want] of Object.entries(expect)) {
    const got = tags.get(Number(tag))?.value;
    if (got !== want) throw new Error(`TIFF tag ${tag} is ${got}, expected ${want}`);
  }
  const tilesX = Math.ceil(SRC_W / TILE);
  const tilesY = Math.ceil(SRC_H / TILE);
  const offsets = read(tags.get(324).value, tilesX * tilesY * 4);
  const counts = read(tags.get(325).value, tilesX * tilesY * 4);

  const w = SRC_W / BLOCK;
  const h = SRC_H / BLOCK;
  const sum = new Float64Array(w * h);
  const cnt = new Uint32Array(w * h);
  const cell = Buffer.alloc(4);
  for (let ty = 0; ty < tilesY; ty++) {
    for (let tx = 0; tx < tilesX; tx++) {
      const k = ty * tilesX + tx;
      const raw = inflateSync(read(offsets.readUInt32LE(k * 4), counts.readUInt32LE(k * 4)));
      for (let r = 0; r < TILE; r++) {
        const y = ty * TILE + r;
        if (y >= SRC_H) break;
        // Predictor 3: each row is byte-differenced, then holds the most significant byte of
        // every sample, then the next, … — so a sample is reassembled big-endian.
        const row = raw.subarray(r * TILE * 4, (r + 1) * TILE * 4);
        for (let i = 1; i < row.length; i++) row[i] = (row[i] + row[i - 1]) & 0xff;
        for (let c = 0; c < TILE; c++) {
          const x = tx * TILE + c;
          if (x >= SRC_W) break;
          cell[0] = row[c];
          cell[1] = row[c + TILE];
          cell[2] = row[c + 2 * TILE];
          cell[3] = row[c + 3 * TILE];
          const v = cell.readFloatBE(0);
          if (!(v > -20000)) continue; // the file's no-data value is -99999
          const o = Math.floor(y / BLOCK) * w + Math.floor(x / BLOCK);
          sum[o] += v;
          cnt[o]++;
        }
      }
    }
  }
  const grid = new Float32Array(w * h);
  for (let i = 0; i < grid.length; i++) grid[i] = cnt[i] ? sum[i] / cnt[i] : 0;
  return { grid, w, h };
}

// ---- marching squares --------------------------------------------------------------------------

/** Closed rings bounding { v >= level }, in map units. Rings are joined by edge id rather than by
 *  float coordinates, so a ring never fails to close over a rounding difference. */
function superlevelRings(grid, w, h, level) {
  // Two rings of padding. The outer one is "outside", so every ring closes. The inner one repeats
  // the grid's own edge (wrapped east-west, the pole rows repeated), and both lie past the map's
  // edge and are clamped onto it — so a band reaches the edge exactly. With "outside" next to the
  // grid instead, every band stopped half a cell short of 180°, and the page's wrapped second
  // world showed that sliver as a seam of the deepest-water colour.
  const PW = w + 4;
  const PH = h + 4;
  const OUTSIDE = -1e9;
  const val = (i, j) => {
    if (i <= 0 || j <= 0 || i >= PW - 1 || j >= PH - 1) return OUTSIDE;
    const si = (i - 2 + w) % w;
    const sj = Math.max(0, Math.min(h - 1, j - 2));
    return grid[sj * w + si];
  };
  // Padded point (i, j) is the centre of source cell (i-2, j-2), clamped onto the map — east-west
  // a hair past it, so the two worlds' bands overlap at 180° instead of abutting: two shapes that
  // only meet are each anti-aliased at the shared edge, and the ground shows through as a line.
  const px = (i) => Math.max(-EW_OVERLAP, Math.min(MAP_WIDTH + EW_OVERLAP, ((i - 1.5) / w) * MAP_WIDTH));
  const py = (j) => Math.max(0, Math.min(MAP_HEIGHT, ((j - 1.5) / h) * MAP_HEIGHT));
  const H = (i, j) => 2 * (j * PW + i);
  const V = (i, j) => 2 * (j * PW + i) + 1;

  const segA = [];
  const segB = [];
  const ends = new Map();
  const add = (a, b) => {
    const s = segA.length;
    segA.push(a);
    segB.push(b);
    for (const e of [a, b]) {
      const l = ends.get(e);
      if (l) l.push(s);
      else ends.set(e, [s]);
    }
  };
  for (let j = 0; j < PH - 1; j++) {
    for (let i = 0; i < PW - 1; i++) {
      const tl = val(i, j);
      const tr = val(i + 1, j);
      const br = val(i + 1, j + 1);
      const bl = val(i, j + 1);
      const c =
        (tl >= level ? 8 : 0) | (tr >= level ? 4 : 0) | (br >= level ? 2 : 0) | (bl >= level ? 1 : 0);
      if (c === 0 || c === 15) continue;
      const T = H(i, j);
      const B = H(i, j + 1);
      const L = V(i, j);
      const R = V(i + 1, j);
      const centre = (tl + tr + br + bl) / 4 >= level;
      switch (c) {
        case 1: case 14: add(L, B); break;
        case 2: case 13: add(B, R); break;
        case 3: case 12: add(L, R); break;
        case 4: case 11: add(T, R); break;
        case 6: case 9: add(T, B); break;
        case 7: case 8: add(L, T); break;
        // The two saddles: the cell's mean decides whether the inside corners connect.
        case 5: if (centre) { add(L, T); add(B, R); } else { add(L, B); add(T, R); } break;
        case 10: if (centre) { add(T, R); add(L, B); } else { add(L, T); add(B, R); } break;
      }
    }
  }
  const point = (e) => {
    const k = e >> 1;
    const i = k % PW;
    const j = (k - i) / PW;
    const horizontal = (e & 1) === 0;
    const a = val(i, j);
    const b = horizontal ? val(i + 1, j) : val(i, j + 1);
    const t = Math.max(0, Math.min(1, (level - a) / (b - a)));
    const ax = px(i);
    const ay = py(j);
    return horizontal ? [ax + t * (px(i + 1) - ax), ay] : [ax, ay + t * (py(j + 1) - ay)];
  };
  const used = new Uint8Array(segA.length);
  const rings = [];
  for (let s0 = 0; s0 < segA.length; s0++) {
    if (used[s0]) continue;
    used[s0] = 1;
    const ring = [point(segA[s0])];
    let edge = segB[s0];
    let s = s0;
    for (;;) {
      ring.push(point(edge));
      const pair = ends.get(edge);
      const next = pair[0] === s ? pair[1] : pair[0];
      if (next === undefined || used[next]) break;
      used[next] = 1;
      edge = segA[next] === edge ? segB[next] : segA[next];
      s = next;
    }
    if (ring.length >= 3) rings.push(ring);
  }
  return rings;
}

// ---- simplification and output (as in gen-world-outline.mjs) -----------------------------------

function segDist(p, a, b) {
  const dx = b[0] - a[0];
  const dy = b[1] - a[1];
  const len2 = dx * dx + dy * dy;
  let t = len2 === 0 ? 0 : ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2;
  t = Math.max(0, Math.min(1, t));
  return Math.hypot(a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
}

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
  return points.filter((_, i) => keep[i]);
}

function area(ring) {
  let a = 0;
  for (let i = 0, j = ring.length - 1; i < ring.length; j = i++) {
    a += (ring[j][0] + ring[i][0]) * (ring[j][1] - ring[i][1]);
  }
  return Math.abs(a / 2);
}

function fmt(n) {
  let s = n.toFixed(1);
  if (s.endsWith('.0')) s = s.slice(0, -2);
  return s === '-0' ? '0' : s;
}

function levelPath(grid, w, h, level) {
  const rings = superlevelRings(grid, w, h, level);
  let d = '';
  let kept = 0;
  for (const ring of rings) {
    const s = rdp(ring, TOL);
    if (s.length < 3 || area(s) < MIN_AREA) continue;
    const pts = [];
    let last = null;
    for (const p of s) {
      const q = `${fmt(p[0])} ${fmt(p[1])}`;
      if (q !== last) pts.push(q);
      last = q;
    }
    if (pts.length > 1 && pts[0] === pts[pts.length - 1]) pts.pop();
    if (pts.length < 3) continue;
    d += `M${pts.join('L')}Z`;
    kept++;
  }
  return { level, d, rings: rings.length, kept };
}

const { grid, w, h } = readGrid();
const sea = SEA_LEVELS.map((l) => levelPath(grid, w, h, l));
const land = LAND_LEVELS.map((l) => levelPath(grid, w, h, l));
for (const b of [...sea, ...land]) {
  console.log(
    `${String(b.level).padStart(6)} m  rings ${b.rings}, kept ${b.kept}, ${(b.d.length / 1024).toFixed(0)} KB`,
  );
}
const json = JSON.stringify({
  sea: sea.map(({ level, d }) => ({ level, d })),
  land: land.map(({ level, d }) => ({ level, d })),
});
console.log(
  `total ${(json.length / 1024).toFixed(0)} KB, gzip ${(gzipSync(json).length / 1024).toFixed(0)} KB`,
);
if (!DRY) {
  writeFileSync(OUT, json);
  console.log(`wrote ${OUT}`);
}
