import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import relief from './geoRelief.json';
import {
  RELIEF_LAND_LEVELS,
  RELIEF_LEGEND,
  RELIEF_SEA_LEVELS,
  reliefLayers,
  type GeoRelief,
} from './geoRelief';

const data = relief as GeoRelief;
const css = readFileSync(new URL('./GeoMapPage.css', import.meta.url), 'utf8');
const tokens = readFileSync(new URL('../styles/tokens.css', import.meta.url), 'utf8');

/** Map units for a place, as `geoProjection.ts::project` computes them. */
function at(lon: number, lat: number): [number, number] {
  return [((lon + 180) / 360) * 720, ((90 - lat) / 180) * 360];
}

/** Whether a point is inside an evenodd path made only of `M…L…Z` rings. */
function inside(d: string, [x, y]: [number, number]): boolean {
  let hit = false;
  for (const ring of d.split('Z')) {
    if (!ring) continue;
    const pts = ring
      .slice(1)
      .split('L')
      .map((p) => p.split(' ').map(Number));
    for (let i = 0, j = pts.length - 1; i < pts.length; j = i++) {
      const [xi, yi] = pts[i];
      const [xj, yj] = pts[j];
      if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) hit = !hit;
    }
  }
  return hit;
}

const band = (level: number) => {
  const b = [...data.sea, ...data.land].find((x) => x.level === level);
  if (!b) throw new Error(`no band at ${level} m`);
  return b.d;
};

describe('the generated relief (ADR-220)', () => {
  it('holds exactly the levels the page has colours for, in painting order', () => {
    expect(data.sea.map((b) => b.level)).toEqual([...RELIEF_SEA_LEVELS]);
    expect(data.land.map((b) => b.level)).toEqual([...RELIEF_LAND_LEVELS]);
    for (const b of [...data.sea, ...data.land]) expect(b.d).toMatch(/^M[-\d. L]+Z/);
  });

  // Points sit well inside their bands: the generator averages 0.1° cells, and a point near an
  // edge would be testing the simplification rather than the data.
  it.each([
    ['central Tibet', 89, 33.5, 4500, null],
    ['the plains east of Denver', -104.5, 39.75, 1000, 2000],
    ['the central Sahara', 10, 24, 500, 2000],
    ['the North Sea', 3, 55, -200, 200],
    ['the abyssal Pacific', -140, 30, -5000, -3000],
    ['the Mariana Trench', 142.2, 11.35, null, -5000],
  ] as const)('puts %s between its levels', (_, lon, lat, above, below) => {
    if (above !== null) expect(inside(band(above), at(lon, lat))).toBe(true);
    if (below !== null) expect(inside(band(below), at(lon, lat))).toBe(false);
  });

  // The second world is drawn one width to the right (ADR-218), so a band that stops short of
  // 0 or 720 leaves a seam of the deepest-water colour down the map. The generator once did.
  it('reaches both edges of the map, so the wrapped world shows no seam', () => {
    for (const level of [-5000, -200, 200]) {
      const xs = [...band(level).matchAll(/[ML](-?[\d.]+) /g)].map((m) => Number(m[1]));
      // A hair past both edges, so the two copies overlap rather than meet.
      expect([Math.min(...xs), Math.max(...xs)], `${level} m`).toEqual([-0.1, 720.1]);
    }
  });

  it('leaves Tokyo below the first land level', () => {
    expect(inside(band(200), at(139.75, 35.68))).toBe(false);
  });
});

describe('the relief colours', () => {
  const layers = reliefLayers(data);
  const classes = [
    'geopage-relief-sea-0',
    'geopage-relief-lowland',
    ...layers.sea.map((l) => l.className),
    ...layers.land.map((l) => l.className),
    ...RELIEF_LEGEND.map((l) => l.className),
  ];

  it.each([...new Set(classes)])('%s is painted by a token both themes define', (cls) => {
    const token = `--geo-relief-${cls.replace('geopage-relief-', '')}`;
    expect(css).toContain(`.${cls} {`);
    expect(css).toContain(`var(${token})`);
    // Once in the light block and once in the dark one.
    expect(tokens.split(`${token}:`).length - 1).toBe(2);
  });
});
