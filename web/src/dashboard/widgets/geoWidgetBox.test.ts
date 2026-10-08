// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { MAP_HEIGHT, MAP_WIDTH, project } from '../../pages/geoProjection';
import { MIN_BOX_H, MIN_BOX_W, geoWidgetBox, loadOutline, pinRadius } from './geoWidgetBox';

const inside = (box: ReturnType<typeof geoWidgetBox>, lat: number, lon: number) => {
  const p = project(lat, lon);
  return p.x >= box.x && p.x <= box.x + box.w && p.y >= box.y && p.y <= box.y + box.h;
};

describe('geoWidgetBox', () => {
  it('shows the whole world when no site has a location', () => {
    expect(geoWidgetBox([])).toEqual({ x: 0, y: 0, w: MAP_WIDTH, h: MAP_HEIGHT });
  });

  // One site has a zero-sized bounding box; the window must still show land around it.
  it('gives a single site the minimum window, centred on it', () => {
    const tokyo = { latitude: 35.68, longitude: 139.69 };
    const box = geoWidgetBox([tokyo]);
    expect(box.w).toBe(MIN_BOX_W);
    expect(box.h).toBe(MIN_BOX_H);
    const p = project(tokyo.latitude, tokyo.longitude);
    expect(box.x + box.w / 2).toBeCloseTo(p.x);
    expect(box.y + box.h / 2).toBeCloseTo(p.y);
  });

  // The old widget normalized min/max into the box, so two sites always sat at the corners. Here a
  // coordinate means one place: the window contains both, with room around them.
  it('contains every site, with space at the edges', () => {
    const sites = [
      { latitude: 35.68, longitude: 139.69 },
      { latitude: 48.86, longitude: 2.35 },
    ];
    const box = geoWidgetBox(sites);
    for (const s of sites) expect(inside(box, s.latitude, s.longitude)).toBe(true);
    const a = project(35.68, 139.69);
    expect(box.x + box.w).toBeGreaterThan(a.x);
  });

  it('slides a window near the edge of the map back inside it rather than cutting it', () => {
    const box = geoWidgetBox([{ latitude: 70, longitude: 179 }]);
    expect(box.x + box.w).toBeLessThanOrEqual(MAP_WIDTH);
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(inside(box, 70, 179)).toBe(true);
  });

  it('never grows past the map', () => {
    const box = geoWidgetBox([
      { latitude: -80, longitude: -179 },
      { latitude: 80, longitude: 179 },
    ]);
    expect(box).toEqual({ x: 0, y: 0, w: MAP_WIDTH, h: MAP_HEIGHT });
  });

  it('sizes a pin by the window, so it reads the same at every zoom', () => {
    expect(pinRadius({ x: 0, y: 0, w: 600, h: 300 })).toBeCloseTo(
      10 * pinRadius({ x: 0, y: 0, w: 60, h: 30 }),
    );
  });
});

describe('loadOutline', () => {
  it('hands over the generated coastline, and the same one on every call', async () => {
    const first = await loadOutline();
    expect(first.land.length).toBeGreaterThan(100);
    expect(first.lakes.length).toBeGreaterThan(0);
    expect(await loadOutline()).toBe(first);
  });
});
