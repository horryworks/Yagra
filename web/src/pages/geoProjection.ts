// SPDX-License-Identifier: AGPL-3.0-only
// Where a site's latitude/longitude lands on the Geo map, and how the view is fitted to the pins.
//
// A `.ts` file rather than logic inside the page, because Vitest never runs `.tsx` — the same split
// `TopologyMap` makes between `layout.ts`/`fitView.ts` and its component. Everything here is a pure
// function of numbers, which is the half that can be silently wrong.
//
// ⚠️ **This is an absolute projection, and it must be.** Normalizing min/max lat/lon into the box
// turns the map into a scatter plot where two sites always land at opposite corners, and every pin
// then sits on the wrong country, which reads as an operator typo rather than a bug. The page and
// the Geo map *widget* (`dashboard/widgets/sites.tsx`) both draw a coastline, so both project
// through here and a coordinate means one place.

/** A point in the map's own coordinate space (the same space `worldOutline` is drawn in). */
export interface Point {
  x: number;
  y: number;
}

/** The map's intrinsic size. Equirectangular: 360° of longitude by 180° of latitude, at 2 units per
 *  degree, so the outline path and this projection share one grid. */
export const MAP_WIDTH = 720;
export const MAP_HEIGHT = 360;

/**
 * Project WGS-84 degrees to map coordinates (equirectangular / plate carrée).
 *
 * Good enough on purpose: this is a "where are my sites" overview, not a navigation chart, and the
 * distortion equirectangular introduces at high latitudes costs nothing at a pin's scale. It is
 * also the projection whose inverse is one subtraction, which keeps the click-to-select maths
 * honest.
 *
 * Out-of-range input is clamped rather than rejected. A group's coordinates are operator-entered
 * and already validated on write; clamping here means a bad row that slipped in draws at the edge
 * instead of dragging the whole viewport off-screen.
 */
export function project(lat: number, lon: number): Point {
  const clampedLon = Math.max(-180, Math.min(180, lon));
  const clampedLat = Math.max(-90, Math.min(90, lat));
  return {
    x: ((clampedLon + 180) / 360) * MAP_WIDTH,
    // Latitude increases north but SVG y increases downward, so this inverts.
    y: ((90 - clampedLat) / 180) * MAP_HEIGHT,
  };
}

/** An axis-aligned box in map coordinates. */
export interface Bounds {
  minX: number;
  minY: number;
  maxX: number;
  maxY: number;
}

/** Anything with coordinates — a node group, in practice. Both may be absent (most groups have no
 *  location set), which is why the callers filter first. */
export interface Placeable {
  latitude?: number | null;
  longitude?: number | null;
}

/** A placeable that definitely has both coordinates. */
export interface Placed {
  latitude: number;
  longitude: number;
}

/** Keep only the entries that carry both coordinates.
 *
 *  Both, not either: a group with a latitude and no longitude cannot be drawn, and defaulting the
 *  missing half to zero would put it in the Gulf of Guinea — a real place, confidently wrong. */
export function placedOnly<T extends Placeable>(items: T[]): (T & Placed)[] {
  return items.filter(
    (g): g is T & Placed => typeof g.latitude === 'number' && typeof g.longitude === 'number',
  );
}

/** The bounding box of a set of projected pins, or `null` when there is nothing to bound. */
export function geoBounds(items: Placed[]): Bounds | null {
  if (items.length === 0) return null;
  const pts = items.map((g) => project(g.latitude, g.longitude));
  return {
    minX: Math.min(...pts.map((p) => p.x)),
    minY: Math.min(...pts.map((p) => p.y)),
    maxX: Math.max(...pts.map((p) => p.x)),
    maxY: Math.max(...pts.map((p) => p.y)),
  };
}

/** The pan/zoom transform the map group is rendered with. Same shape as the topology map's `View`,
 *  deliberately — the pointer handlers there are the ones this page reuses. */
export interface GeoView {
  tx: number;
  ty: number;
  scale: number;
}

/** The zoom ceiling. There is no fixed floor: how far out the map may go depends on the pane
 *  (`minGeoScale`), because the only meaningful floor is "the whole world, and no more".
 *
 *  240 is the user's request (ADR-188): ten times the old 24, so the sites inside one city stop
 *  overlapping. The world outline is simplified at ≈3.3 km (ADR-127 decision 2), about 14 px here,
 *  so past `GEO_DETAIL_MIN_SCALE` the page draws the fine tiles instead (ADR-207), simplified at
 *  ≈110 m — half a pixel at this ceiling. */
export const MAX_GEO_SCALE = 240;

/** Degrees per side of one fine coastline tile (ADR-207). The generator cuts at the same size
 *  (`TILE_DEG` in `scripts/gen-world-outline.mjs`); `geoTiles.test.ts` checks the files agree. */
export const GEO_TILE_DEG = 10;

/** One tile's side in map units. */
export const GEO_TILE_UNITS = (GEO_TILE_DEG * MAP_WIDTH) / 360;

/** The zoom from which the fine tiles replace the world outline. Below it the world outline's
 *  ≈5.5 km rounding is under two pixels; above it the pane spans few enough tiles (about nine on a
 *  wide pane) that loading and drawing them stays cheap. */
export const GEO_DETAIL_MIN_SCALE = 40;

/**
 * The fine tiles the pane shows, as `"<col>_<row>"` keys (the generator's file names without the
 * `t` and the extension), or none below `GEO_DETAIL_MIN_SCALE`.
 *
 * Whether a tile has a file is not this function's question: tiles of open ocean were never
 * written, and the caller skips keys it has no file for.
 */
export function visibleGeoTiles(v: GeoView, vw: number, vh: number): string[] {
  if (v.scale < GEO_DETAIL_MIN_SCALE || vw <= 0 || vh <= 0) return [];
  const cols = MAP_WIDTH / GEO_TILE_UNITS;
  const rows = MAP_HEIGHT / GEO_TILE_UNITS;
  const toTile = (map: number, last: number) =>
    Math.max(0, Math.min(last, Math.floor(map / GEO_TILE_UNITS)));
  const r0 = toTile(-v.ty / v.scale, rows - 1);
  const r1 = toTile((vh - v.ty) / v.scale, rows - 1);
  // East-west the world repeats (ADR-218), so a pane across the date line wants the last columns
  // and the first ones; north-south it ends, so a row past a pole is the edge row.
  const cs: number[] = [];
  if (geoWraps(v.scale, vw)) {
    const c0 = Math.floor(-v.tx / v.scale / GEO_TILE_UNITS);
    const c1 = Math.floor((vw - v.tx) / v.scale / GEO_TILE_UNITS);
    for (let c = c0; c <= c1; c++) {
      const col = ((c % cols) + cols) % cols;
      if (!cs.includes(col)) cs.push(col);
    }
  } else {
    const c0 = toTile(-v.tx / v.scale, cols - 1);
    const c1 = toTile((vw - v.tx) / v.scale, cols - 1);
    for (let c = c0; c <= c1; c++) cs.push(c);
  }
  const keys: string[] = [];
  for (const c of cs) for (let r = r0; r <= r1; r++) keys.push(`${c}_${r}`);
  return keys;
}

/** Fraction of the viewport fitted pins fill, so pins near the edge are not flush. */
const MARGIN = 0.88;

/**
 * The zoom floor for a pane: the scale at which the whole world just fits ("contain").
 *
 * Any smaller and the map floats in empty space, which is what zooming out used to do on a wide
 * pane (ADR-188). "Fill the pane" (cover) was rejected: on a wide pane it hides the poles even at
 * the floor. An unmeasured pane answers `1` rather than zero or NaN.
 */
export function minGeoScale(vw: number, vh: number): number {
  if (vw <= 0 || vh <= 0) return 1;
  return Math.min(MAX_GEO_SCALE, Math.min(vw / MAP_WIDTH, vh / MAP_HEIGHT));
}

/** Clamp a proposed zoom to what this pane allows. */
export function clampGeoScale(scale: number, vw: number, vh: number): number {
  return Math.min(MAX_GEO_SCALE, Math.max(minGeoScale(vw, vh), scale));
}

/** One axis of `clampGeoView`: a map narrower than the pane is centred; a wider one may move only
 *  as far as keeps its edges at or beyond the pane's. */
function clampAxis(t: number, mapLen: number, paneLen: number): number {
  if (mapLen <= paneLen) return (paneLen - mapLen) / 2;
  return Math.min(0, Math.max(paneLen - mapLen, t));
}

/** How far the world may fall short of the pane's width and still wrap. The pane is never wider
 *  than the world at its height (ADR-188 Inc.2), so at the zoom floor the two are equal — but the
 *  browser rounds the pane's width and height separately, and a world one pixel short of the pane
 *  must not fall back to being centred with a sliver either side. The second copy covers it. */
const WRAP_SLACK_PX = 1;

/**
 * Whether the world repeats east-west at this zoom (ADR-218): it does whenever one world is at
 * least as wide as the pane, which is every zoom on a pane no wider than the world. Narrower than
 * that, the world is centred as before — a pane that shows two worlds side by side is the one look
 * the user ruled out.
 */
export function geoWraps(scale: number, vw: number): boolean {
  return vw > 0 && MAP_WIDTH * scale >= vw - WRAP_SLACK_PX;
}

/** `t` folded into `(-mapLen, 0]`, so a drag in either direction never runs out of world and the
 *  number never grows. `-0` is answered as `0`, which is what a test comparing views expects. */
function wrapAxis(t: number, mapLen: number): number {
  const r = t % mapLen;
  if (r === 0) return 0;
  return r > 0 ? r - mapLen : r;
}

/**
 * Hold a view inside what this pane can show without showing anything outside the world.
 *
 * Every write to the view goes through here — wheel, pinch, drag, the buttons, Fit, and a view
 * restored from the account (ADR-218), which may have been saved on a pane of another size.
 * Keeping the rule in one function is what stops one of those paths from letting the map escape.
 *
 * North-south the map stops at the poles. East-west it repeats (ADR-218): `tx` is folded into one
 * world's width, and the page draws a second copy of the world one width to the right, which is
 * all a pane at most one world wide can ever need.
 */
export function clampGeoView(v: GeoView, vw: number, vh: number): GeoView {
  if (vw <= 0 || vh <= 0) return v;
  const scale = clampGeoScale(v.scale, vw, vh);
  const mapW = MAP_WIDTH * scale;
  return {
    scale,
    tx: geoWraps(scale, vw) ? wrapAxis(v.tx, mapW) : clampAxis(v.tx, mapW, vw),
    ty: clampAxis(v.ty, MAP_HEIGHT * scale, vh),
  };
}

/**
 * Where to draw something at map `x` so that it is on screen (ADR-218): in the first copy of the
 * world, or one width to the right in the second.
 *
 * Each pin is drawn **once**, at the copy the pane shows it in. Drawing it in both copies would
 * double every pin's Tab stop and test id, and a pane at most one world wide shows any point at
 * most once. A view that does not wrap has its left edge at or left of zero, so nothing moves.
 */
export function wrapPinX(x: number, v: GeoView): number {
  return x < -v.tx / v.scale ? x + MAP_WIDTH : x;
}

/** The inverse of `project`: map coordinates back to degrees. Longitude comes back in
 *  `[-180, 180)` whichever copy of the world `x` was in. */
export function unproject(p: Point): { lat: number; lon: number } {
  const lon = (p.x / MAP_WIDTH) * 360 - 180;
  return {
    lat: 90 - (p.y / MAP_HEIGHT) * 180,
    lon: ((((lon + 180) % 360) + 360) % 360) - 180,
  };
}

/**
 * Where the Geo map is looking, independent of the pane it is drawn in (ADR-218): the longitude
 * and latitude at the pane's centre, and the zoom as a multiple of the pane's floor.
 *
 * This is what the account stores, rather than `tx`/`ty`/`scale`, because those are pixels of one
 * pane: restored on another window or on a phone they point somewhere else (the cost ADR-134
 * decision 7 accepted for a value kept per tab). The floor is "one world across the pane", so a
 * multiple of it is a span of longitude and means the same on any screen.
 */
export interface GeoPlace {
  lon: number;
  lat: number;
  zoom: number;
}

/** The place a view shows in this pane, or `null` for an unmeasured pane. */
export function placeFromView(v: GeoView, vw: number, vh: number): GeoPlace | null {
  if (vw <= 0 || vh <= 0 || !(v.scale > 0)) return null;
  const c = unproject({ x: (vw / 2 - v.tx) / v.scale, y: (vh / 2 - v.ty) / v.scale });
  return { lon: c.lon, lat: c.lat, zoom: v.scale / minGeoScale(vw, vh) };
}

/** The view that shows `place` in this pane, held inside it like every other view. */
export function viewFromPlace(place: GeoPlace, vw: number, vh: number): GeoView {
  if (vw <= 0 || vh <= 0) return { tx: 0, ty: 0, scale: 1 };
  const scale = clampGeoScale(place.zoom * minGeoScale(vw, vh), vw, vh);
  const c = project(place.lat, place.lon);
  return clampGeoView({ tx: vw / 2 - c.x * scale, ty: vh / 2 - c.y * scale, scale }, vw, vh);
}

/** The largest zoom multiple a stored place may carry. Above the real ceiling for any pane (240
 *  over a floor of about 0.4 on a phone), so it only refuses a value no pane could have written. */
const MAX_PLACE_ZOOM = 10_000;

/**
 * Read a stored place out of the account's document, or `null` when it is not one.
 *
 * Defensive because the document is opaque to the backend (ADR-058): anything that is not three
 * finite numbers in range is dropped, and the page then fits the pins as if nothing were stored.
 */
export function adoptGeoPlace(raw: unknown): GeoPlace | null {
  if (raw == null || typeof raw !== 'object') return null;
  const { lon, lat, zoom } = raw as Record<string, unknown>;
  const ok = (n: unknown, lo: number, hi: number): n is number =>
    typeof n === 'number' && Number.isFinite(n) && n >= lo && n <= hi;
  if (!ok(lon, -180, 180) || !ok(lat, -90, 90) || !ok(zoom, 0, MAX_PLACE_ZOOM) || zoom === 0) {
    return null;
  }
  return { lon, lat, zoom };
}

/**
 * Zoom by `factor` around a point in pane coordinates, keeping that point fixed, then clamp.
 * The wheel passes the cursor; the buttons pass the pane's centre.
 */
export function zoomGeoView(
  v: GeoView,
  factor: number,
  cx: number,
  cy: number,
  vw: number,
  vh: number,
): GeoView {
  const scale = clampGeoScale(v.scale * factor, vw, vh);
  const k = scale / v.scale;
  return clampGeoView({ scale, tx: cx - (cx - v.tx) * k, ty: cy - (cy - v.ty) * k }, vw, vh);
}

/**
 * Fit the whole world into the viewport, centred.
 *
 * The answer when there are no pins — and the right one: an operator who has set no coordinates
 * should see the map and understand what it is for, not an empty pane. It is exactly the zoom
 * floor, with no margin: a margin would put it below the floor and the clamp would move it.
 */
export function fitWorld(vw: number, vh: number): GeoView {
  if (vw <= 0 || vh <= 0) return { tx: 0, ty: 0, scale: 1 };
  return clampGeoView({ tx: 0, ty: 0, scale: minGeoScale(vw, vh) }, vw, vh);
}

/**
 * Fit the viewport to the pins, falling back to the whole world when there are none.
 *
 * A **single** pin (or several at one place) has a zero-sized bounding box, which would divide by
 * zero and put the map at `NaN` — rendering nothing, with no error anywhere. That case gets a fixed
 * comfortable zoom centred on the pin instead, which is also what an operator wants to see.
 */
export function fitPins(items: Placed[], vw: number, vh: number): GeoView {
  if (vw <= 0 || vh <= 0) return { tx: 0, ty: 0, scale: 1 };
  const b = geoBounds(items);
  if (!b) return fitWorld(vw, vh);
  const w = b.maxX - b.minX;
  const h = b.maxY - b.minY;
  const cx = (b.minX + b.maxX) / 2;
  const cy = (b.minY + b.maxY) / 2;
  // A degenerate box means every site is effectively in one place.
  const scale =
    w <= 0 || h <= 0
      ? clampGeoScale(4, vw, vh)
      : clampGeoScale(Math.min((vw / w) * MARGIN, (vh / h) * MARGIN), vw, vh);
  // Centred on the pins, then clamped: a site near a pole is shown at the edge of the pane rather
  // than beside empty space. One near the date line stays centred, with the next world beside it
  // (ADR-218). The box is still the one that does not cross the date line, so sites on both sides
  // of the Pacific are framed across the Atlantic, as before.
  return clampGeoView({ tx: vw / 2 - cx * scale, ty: vh / 2 - cy * scale, scale }, vw, vh);
}
