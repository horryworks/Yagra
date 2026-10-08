// SPDX-License-Identifier: AGPL-3.0-only
// The part of the world the Geo map widget shows: an SVG viewBox around its pins, in the same
// equirectangular grid the coastline is drawn in (`pages/geoProjection.ts`).
//
// The widget used to be a scatter plot with no map behind it — min/max lat/lon normalized into the
// box, so two sites always sat at opposite corners. With a coastline behind the pins that projection
// is wrong (every pin lands on the wrong country, the warning `geoProjection.ts` carries), so the
// widget projects absolutely, as the page does, and only chooses which window of the map to show.
// A pure function in a `.ts` because Vitest never runs a `.tsx`.

import { MAP_HEIGHT, MAP_WIDTH, geoBounds, type Placed } from '../../pages/geoProjection';

/** An SVG viewBox in map units. */
export interface GeoBox {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** The narrowest window, in map units (2 per degree): about 30° by 15°. One site, or several in
 *  one city, would otherwise zoom to a point and show no coastline at all. */
export const MIN_BOX_W = 60;
export const MIN_BOX_H = 30;

/** Space around the pins, as a fraction of their span, so an edge pin is not flush with the frame. */
const PAD = 0.2;

/**
 * The window around `items`, or the whole world when there are none.
 *
 * Padded, widened to the minimum, and then slid back inside the map rather than cut: a site near
 * the date line or a pole is shown at the edge of the frame with land beside it, not beside a
 * band of nothing. A window wider or taller than the map is the map.
 */
export function geoWidgetBox(items: Placed[]): GeoBox {
  const b = geoBounds(items);
  if (!b) return { x: 0, y: 0, w: MAP_WIDTH, h: MAP_HEIGHT };
  const w = Math.min(MAP_WIDTH, Math.max(MIN_BOX_W, (b.maxX - b.minX) * (1 + 2 * PAD)));
  const h = Math.min(MAP_HEIGHT, Math.max(MIN_BOX_H, (b.maxY - b.minY) * (1 + 2 * PAD)));
  const cx = (b.minX + b.maxX) / 2;
  const cy = (b.minY + b.maxY) / 2;
  const x = Math.max(0, Math.min(MAP_WIDTH - w, cx - w / 2));
  const y = Math.max(0, Math.min(MAP_HEIGHT - h, cy - h / 2));
  return { x, y, w, h };
}

/** A pin's radius in map units, so it is the same size on screen whatever window is shown. */
export function pinRadius(box: GeoBox): number {
  return Math.max(box.w, box.h) * 0.012;
}

/** The coastline as the widget draws it. */
export interface Outline {
  land: readonly string[];
  lakes: readonly string[];
}

let outlineLoad: Promise<Outline> | null = null;

/** The coastline, fetched the first time a Geo map widget mounts and shared after that. It is
 *  ~240 KB compressed, so a dashboard without the widget never asks for it (the Geo map page
 *  imports the same module, so the two share one chunk). A failed fetch is not cached, so the next
 *  mount asks again. */
export function loadOutline(): Promise<Outline> {
  outlineLoad ??= import('../../pages/worldOutline').then(
    (m) => ({ land: m.WORLD_OUTLINE, lakes: m.WORLD_LAKES }),
    (e: unknown) => {
      outlineLoad = null;
      throw e;
    },
  );
  return outlineLoad;
}
