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
 *  overlapping. ⚠️ The coastline is simplified at ≈3.3 km (ADR-127 decision 2), which is about 14 px
 *  here — the outline turns visibly angular near the ceiling. The pins stay exact; the outline is
 *  context, and the resolution was accepted as the price of zooming in on a site. */
export const MAX_GEO_SCALE = 240;

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

/**
 * Hold a view inside what this pane can show without showing anything outside the world.
 *
 * Every write to the view goes through here — wheel, pinch, drag, the buttons, Fit, and a view
 * restored from the session (ADR-134), which may have been saved on a pane of another size.
 * Keeping the rule in one function is what stops one of those paths from letting the map escape.
 */
export function clampGeoView(v: GeoView, vw: number, vh: number): GeoView {
  if (vw <= 0 || vh <= 0) return v;
  const scale = clampGeoScale(v.scale, vw, vh);
  return {
    scale,
    tx: clampAxis(v.tx, MAP_WIDTH * scale, vw),
    ty: clampAxis(v.ty, MAP_HEIGHT * scale, vh),
  };
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
  // Centred on the pins, then clamped: a site near the date line or a pole is shown at the edge of
  // the pane rather than beside empty space.
  return clampGeoView({ tx: vw / 2 - cx * scale, ty: vh / 2 - cy * scale, scale }, vw, vh);
}
