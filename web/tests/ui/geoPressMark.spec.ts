// SPDX-License-Identifier: AGPL-3.0-only
// The Geo map marker whose bubble is open carries a ring, and a ripple runs once from it when the
// press opens the bubble (ADR-206 Inc.2).
//
// Why Tier1: there is no judgement to unit-test — what can be wrong is the page drawing the mark on
// the right marker, keeping it a fixed size on screen, taking it away when the bubble closes, and
// leaving the ripple out for an operator who asked for less motion. None of that runs in Vitest.
//
// Every body is the generated one, patched (testing.md).

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const id = (n: number) =>
  `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const TOKYO = id(1);
const OSAKA = id(2);
// Two sites on one coordinate: always one marker.
const SAME_A = id(3);
const SAME_B = id(4);

function folders(): Json {
  const [t] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const g = (gid: string, name: string, lat: number, lon: number) => ({
    ...t,
    id: gid,
    name: `${MOCK_PREFIX}${name}`,
    parent_id: null,
    origin: null,
    latitude: lat,
    longitude: lon,
    geo_group: gid,
  });
  return [
    g(TOKYO, 'tokyo', 35.68, 139.76),
    g(OSAKA, 'osaka', 34.69, 135.5),
    g(SAME_A, 'london-a', 51.5, -0.12),
    g(SAME_B, 'london-b', 51.5, -0.12),
  ] as unknown as Json;
}

function summary(): Json {
  const none = {
    critical: 0,
    maintenance: 0,
    ok: 0,
    unknown: 0,
    unreachable: 0,
    warning: 0,
  };
  return {
    groups: Object.fromEntries(
      [TOKYO, OSAKA, SAME_A, SAME_B].map((g) => [g, { ...none, ok: 2 }]),
    ),
  } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': folders(),
      '/api/v1/fleet/group-summary': summary(),
    },
  },
});

/** The ring's diameter on screen, in CSS pixels. */
async function ringSize(page: Page): Promise<number> {
  const box = (await page.locator('.geopage-pin-ring').boundingBox())!;
  return box.width;
}

test('the pressed pin carries a ring until its bubble closes, the same size at any zoom', async ({
  page,
  errors,
}) => {
  await page.goto('/topology/geo');
  const pins = page.getByTestId('geo-map-pin');
  await expect(pins).toHaveCount(2);
  await expect(page.locator('.geopage-pin-ring')).toHaveCount(0);

  await pins.first().click();
  await expect(page.getByRole('dialog', { name: /tokyo|osaka/ })).toBeVisible();
  // One ring, on the pressed pin, with its ripple beside it.
  await expect(page.locator('.geopage-pin-ring')).toHaveCount(1);
  await expect(pins.first().locator('.geopage-pin-ring')).toHaveCount(1);
  await expect(pins.first().locator('.geopage-pin-ripple')).toHaveCount(1);
  const animation = await pins
    .first()
    .locator('.geopage-pin-ripple')
    .evaluate((el) => getComputedStyle(el).animationName);
  expect(animation).toBe('geopage-ripple');
  const before = await ringSize(page);
  // About 11.5 px in radius: outside the halo (9.5), nowhere near the pane's size.
  expect(before).toBeGreaterThan(18);
  expect(before).toBeLessThan(30);

  await page.keyboard.press('Escape');
  await expect(page.locator('.geopage-pin-ring')).toHaveCount(0);

  // Zoom in around the pin and press it again: the ring is still the same size on screen.
  const at = (await pins.first().boundingBox())!;
  await page.mouse.move(at.x + at.width / 2, at.y + at.height / 2);
  for (let i = 0; i < 6; i++) await page.mouse.wheel(0, -200);
  await pins.first().click();
  await expect(page.locator('.geopage-pin-ring')).toHaveCount(1);
  expect(Math.abs((await ringSize(page)) - before)).toBeLessThan(1);

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test('a pressed cluster carries the ring, and a row moves it nowhere it cannot be drawn', async ({
  page,
  errors,
}) => {
  await page.goto('/topology/geo');
  const cluster = page.getByTestId('geo-map-cluster');
  await expect(cluster).toHaveCount(1);
  await cluster.click();
  await expect(cluster.locator('.geopage-pin-ring')).toHaveCount(1);
  await expect(page.locator('.geopage-pin-ring')).toHaveCount(1);

  // Picking a site from the list opens its bubble; that site is inside the cluster, so no marker
  // of its own carries the ring.
  await page
    .getByRole('dialog')
    .getByRole('button', { name: new RegExp(`${MOCK_PREFIX}london-a`) })
    .click();
  await expect(
    page.getByRole('dialog', { name: `${MOCK_PREFIX}london-a` }),
  ).toBeVisible();
  await expect(page.locator('.geopage-pin-ring')).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test.describe('with reduced motion', () => {
  test('the ring is drawn and the ripple does not run', async ({
    page,
    errors,
  }) => {
    await page.emulateMedia({ reducedMotion: 'reduce' });
    await page.goto('/topology/geo');
    const pin = page.getByTestId('geo-map-pin').first();
    await pin.click();
    await expect(pin.locator('.geopage-pin-ring')).toHaveCount(1);
    const animation = await pin
      .locator('.geopage-pin-ripple')
      .evaluate((el) => getComputedStyle(el).animationName);
    expect(animation).toBe('none');

    expect(errors.uncaught).toEqual([]);
    expect(errors.logged).toEqual([]);
  });
});
