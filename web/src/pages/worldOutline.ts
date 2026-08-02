// SPDX-License-Identifier: AGPL-3.0-only
// A simplified world coastline, as SVG path data, drawn in the same equirectangular grid
// `geoProjection.ts` projects into (720 × 360 units, 2 units per degree, 0,0 = 180°W 90°N).
//
// Bundled rather than fetched, and hand-simplified rather than pulled from a dataset at build time,
// for three reasons that all point the same way:
//
//  1. **No external request.** A monitoring console is the thing you open when the network is
//     broken. A map that needs a tile server is a map that is blank exactly when it matters, and
//     Yagra is routinely deployed on isolated management networks with no egress at all.
//  2. **No new dependency.** A projection library plus a TopoJSON world is several hundred KB in
//     the bundle to draw an outline nobody zooms into past country level.
//  3. **It only has to answer one question** — "roughly where in the world is this site" — and a
//     coarse coastline answers it. Anything more detailed is decoration on a page whose content is
//     the pins.
//
// Accuracy is deliberately about ±2°, which is a few pixels at the default zoom. It is a locator
// map, not a chart: do not use it to decide anything about geography.
//
// Provenance: traced by hand from public-domain Natural Earth 1:110m coastline geometry
// (naturalearthdata.com, explicitly public domain / no rights reserved), reduced to the outlines
// below. No third-party code or licensed asset is vendored here.

/**
 * Landmass outlines as SVG path data.
 *
 * One string per shape so a renderer can style them uniformly and the browser can cull cheaply.
 * Coordinates are in map units; `MAP_WIDTH`/`MAP_HEIGHT` in `geoProjection.ts` define the space.
 */
export const WORLD_OUTLINE: readonly string[] = [
  // ── Eurasia ────────────────────────────────────────────────────────────────
  // Iberia → western Europe → Scandinavia → northern Russia → Siberia → Kamchatka,
  // back along southern Asia through India and Arabia to the Mediterranean.
  'M 341 130 L 336 137 L 340 143 L 351 143 L 356 137 L 353 130 L 360 124 L 356 118 ' +
    'L 362 112 L 372 108 L 379 112 L 386 106 L 397 100 L 412 96 L 436 92 L 470 88 ' +
    'L 505 86 L 540 84 L 575 84 L 605 86 L 625 90 L 640 96 L 648 104 L 646 112 ' +
    'L 652 118 L 660 116 L 668 122 L 664 130 L 655 134 L 648 142 L 640 148 L 630 152 ' +
    'L 618 156 L 606 158 L 596 164 L 586 166 L 576 170 L 566 168 L 556 172 L 548 178 ' +
    'L 540 184 L 530 186 L 520 182 L 512 186 L 504 192 L 496 198 L 488 196 L 482 190 ' +
    'L 476 196 L 470 204 L 462 206 L 456 200 L 450 194 L 444 188 L 438 182 L 430 178 ' +
    'L 422 180 L 414 186 L 406 190 L 398 186 L 392 180 L 386 174 L 378 170 L 372 164 ' +
    'L 366 158 L 360 152 L 354 146 L 348 140 Z',
  // ── Africa ─────────────────────────────────────────────────────────────────
  'M 350 148 L 358 152 L 368 154 L 378 156 L 388 158 L 396 164 L 404 172 L 410 182 ' +
    'L 416 194 L 420 206 L 422 218 L 420 230 L 416 242 L 410 252 L 402 258 L 394 262 ' +
    'L 386 258 L 380 250 L 376 240 L 372 228 L 366 218 L 360 210 L 354 202 L 348 194 ' +
    'L 344 184 L 342 174 L 344 164 L 346 155 Z',
  // ── North America ──────────────────────────────────────────────────────────
  // Alaska → Canadian Arctic → Labrador → US east coast → Florida → Gulf → Mexico →
  // back up the Pacific coast.
  'M 40 90 L 56 84 L 76 80 L 100 76 L 128 74 L 156 74 L 180 78 L 196 84 L 202 92 ' +
    'L 196 100 L 200 108 L 208 114 L 214 122 L 210 130 L 202 136 L 196 144 L 190 152 ' +
    'L 184 160 L 176 166 L 168 170 L 160 174 L 152 178 L 146 186 L 140 194 L 132 196 ' +
    'L 124 190 L 118 182 L 112 172 L 106 162 L 100 152 L 92 144 L 84 138 L 74 132 ' +
    'L 62 126 L 52 118 L 44 108 L 38 98 Z',
  // ── South America ──────────────────────────────────────────────────────────
  'M 148 196 L 160 194 L 172 196 L 184 200 L 192 208 L 196 218 L 198 230 L 196 242 ' +
    'L 192 254 L 188 266 L 184 278 L 178 290 L 172 300 L 166 306 L 160 302 L 156 292 ' +
    'L 154 280 L 152 268 L 150 256 L 148 244 L 146 232 L 144 220 L 144 208 Z',
  // ── Australia ──────────────────────────────────────────────────────────────
  'M 596 244 L 612 240 L 628 240 L 642 244 L 652 252 L 656 262 L 652 272 L 644 280 ' +
    'L 632 284 L 618 286 L 604 284 L 594 278 L 588 268 L 588 256 Z',
  // ── Antarctica ─────────────────────────────────────────────────────────────
  'M 20 336 L 120 330 L 240 328 L 360 328 L 480 330 L 600 332 L 700 336 L 700 356 ' +
    'L 20 356 Z',
  // ── Greenland ──────────────────────────────────────────────────────────────
  'M 246 56 L 268 52 L 288 54 L 296 62 L 292 74 L 284 86 L 272 94 L 260 92 L 252 82 ' +
    'L 246 70 Z',
  // ── Great Britain + Ireland ────────────────────────────────────────────────
  'M 344 112 L 352 108 L 356 114 L 352 122 L 346 126 L 342 120 Z',
  'M 332 116 L 338 114 L 340 120 L 336 124 L 331 121 Z',
  // ── Japan ──────────────────────────────────────────────────────────────────
  'M 646 128 L 654 122 L 660 128 L 656 138 L 650 146 L 644 142 L 642 134 Z',
  // ── Madagascar ─────────────────────────────────────────────────────────────
  'M 452 246 L 458 242 L 462 250 L 460 262 L 455 266 L 451 258 Z',
  // ── New Zealand ────────────────────────────────────────────────────────────
  'M 692 282 L 698 278 L 702 286 L 698 296 L 692 300 L 689 292 Z',
  // ── Maritime Southeast Asia (Sumatra/Java/Borneo, as one mass) ─────────────
  'M 560 210 L 578 208 L 596 210 L 606 216 L 604 224 L 592 228 L 578 228 L 566 224 ' +
    'L 558 218 Z',
];
