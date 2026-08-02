// SPDX-License-Identifier: AGPL-3.0-only
// Where a site's latitude/longitude lands on the Geo map, and how the view is fitted to the pins.
//
// A `.ts` file rather than logic inside the page, because Vitest never runs `.tsx` — the same split
// `TopologyMap` makes between `layout.ts`/`fitView.ts` and its component. Everything here is a pure
// function of numbers, which is the half that can be silently wrong.
//
// ⚠️ **This is an absolute projection, and it must be.** The Geo map *widget* (`dashboard/widgets/
// sites.tsx`) normalizes min/max lat/lon into its box, which is right for a widget with no map
// behind it: it is a scatter plot, and two sites always land at opposite corners. Reuse that here
// and every pin sits on the wrong country, which reads as an operator typo rather than a bug. The
// page draws a coastline, so a coordinate has to mean one place.

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

/** Zoom bounds. `1` shows the whole world at its intrinsic size; the floor lets a small pane still
 *  fit the world, and the ceiling stops a single site zooming to a meaningless blur of coastline. */
export const MIN_GEO_SCALE = 0.3;
export const MAX_GEO_SCALE = 12;

/** Fraction of the viewport the fitted content fills, so pins near the edge are not flush. */
const MARGIN = 0.88;

/** Clamp a proposed zoom to the bounds a fit could produce. */
export function clampGeoScale(scale: number): number {
  return Math.min(MAX_GEO_SCALE, Math.max(MIN_GEO_SCALE, scale));
}

/**
 * Fit the whole world into the viewport, centred.
 *
 * The answer when there are no pins — and the right one: an operator who has set no coordinates
 * should see the map and understand what it is for, not an empty pane.
 */
export function fitWorld(vw: number, vh: number): GeoView {
  if (vw <= 0 || vh <= 0) return { tx: 0, ty: 0, scale: 1 };
  const scale = clampGeoScale(Math.min(vw / MAP_WIDTH, vh / MAP_HEIGHT) * MARGIN);
  return {
    tx: (vw - MAP_WIDTH * scale) / 2,
    ty: (vh - MAP_HEIGHT * scale) / 2,
    scale,
  };
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
      ? clampGeoScale(4)
      : clampGeoScale(Math.min((vw / w) * MARGIN, (vh / h) * MARGIN));
  return { tx: vw / 2 - cx * scale, ty: vh / 2 - cy * scale, scale };
}
