// SPDX-License-Identifier: AGPL-3.0-only
// The Geo map's elevation colouring switch (ADR-220).
//
// Why Tier1: the bands' contents and their colour tokens are unit-tested (`geoRelief.test.ts`).
// What Vitest cannot run is the page fetching the bands only when the switch is pressed, drawing
// them in place of the grey ground, the switch reaching the account, and — the layering that is
// easiest to get wrong — the fine coastline tiles keeping the colours instead of painting flat
// ocean squares over them.
//
// Every body is the generated one, patched (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const SITE = '00000000-0000-4000-8000-000000000001';

function folders(): Json {
  const [t] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  return [
    {
      ...t,
      id: SITE,
      name: `${MOCK_PREFIX}site-tokyo`,
      parent_id: null,
      origin: null,
      latitude: 35.68,
      longitude: 139.76,
      geo_group: SITE,
    },
  ] as unknown as Json;
}

function summary(): Json {
  const none = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return { groups: { [SITE]: { ...none, ok: 3 } } } as unknown as Json;
}

// The account document as the page last saved it, reset per test — workers reuse this module.
let account: Json = null;
test.beforeEach(() => {
  account = null;
});

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': folders(),
      '/api/v1/fleet/group-summary': summary(),
      '/api/v1/preferences': (_url, request) => {
        if (request.method() === 'PUT') {
          account = request.postDataJSON() as Json;
          return { ok: true };
        }
        return account;
      },
    },
  },
});

const SWITCH = '.geopage-ctl-relief';
const LAND_BAND = 'path.geopage-relief-land-1';

test('the switch colours the map, reaches the account, and turns it grey again', async ({
  page,
  errors,
}) => {
  await page.goto('/topology/geo');
  await expect(page.getByTestId('geo-map-pin')).toHaveCount(1);
  const button = page.locator(SWITCH);
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  await expect(page.locator(LAND_BAND), 'grey by default').toHaveCount(0);
  await expect(page.locator('rect.geopage-ocean')).toHaveCount(1);

  const saved = page.waitForRequest(
    (r) =>
      r.method() === 'PUT' &&
      new URL(r.url()).pathname === '/api/v1/preferences' &&
      (r.postDataJSON() as { geoMapRelief?: unknown } | null)?.geoMapRelief === true,
  );
  await button.click();
  await expect(button).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator(LAND_BAND), 'the bands arrive and are drawn').toHaveCount(1);
  expect((await page.locator(LAND_BAND).getAttribute('d'))?.length ?? 0).toBeGreaterThan(10_000);
  await expect(page.locator('rect.geopage-ocean'), 'the grey ground is gone').toHaveCount(0);
  await expect(page.locator('.geopage-relief-swatch'), 'lowland and six levels').toHaveCount(7);
  await saved;

  await button.click();
  await expect(button).toHaveAttribute('aria-pressed', 'false');
  await expect(page.locator(LAND_BAND)).toHaveCount(0);
  await expect(page.locator('rect.geopage-ocean')).toHaveCount(1);
  await expect(page.locator('.geopage-relief-swatch')).toHaveCount(0);
  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test('zoomed in on the fine coastline, the tiles keep the colours', async ({ page, errors }) => {
  // Opened from the account: the switch on, and the map close over Tokyo Bay, past the zoom at
  // which the fine tiles load (ADR-207).
  account = { geoMapRelief: true, geoMapPlace: { lon: 139.8, lat: 35.5, zoom: 40 } } as Json;
  await page.goto('/topology/geo');
  await expect(page.locator(SWITCH)).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator(LAND_BAND)).toHaveCount(1);
  await expect(
    page.locator('path.geopage-detail-land.geopage-relief-lowland').first(),
    'the fine tiles load and draw their land in the relief colour',
  ).toBeAttached();
  await expect(page.locator('#geopage-relief-tiles rect').first()).toBeAttached();
  // The grey layer paints each loaded square in the ocean colour first; drawn here, it would
  // cover the relief with flat squares.
  await expect(page.locator('rect.geopage-ocean')).toHaveCount(0);
  await expect(page.locator('path.geopage-detail-land:not(.geopage-relief-lowland)')).toHaveCount(0);
  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});
