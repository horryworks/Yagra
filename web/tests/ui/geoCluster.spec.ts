// SPDX-License-Identifier: AGPL-3.0-only
// Geo map sites that overlap on screen are drawn as one marker with their count, and the marker
// lists them (ADR-206).
//
// Why Tier1: which sites group together is unit-tested (`geoCluster.test.ts`). What Vitest cannot
// run is the page drawing the marker at the fitted scale, the list it opens, and a row handing over
// to the site's own bubble. The walk cannot stand in: its generated mock places no two sites close.
//
// Every body is the generated one, patched (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
// Three sites on one coordinate: no zoom can pull them apart.
const SAME_A = id(1);
const SAME_B = id(2);
const SAME_C = id(3);
// Two sites a few metres apart, on the other side of the world: they overlap at the fitted scale
// and separate only when zoomed in.
const NEAR_D = id(4);
const NEAR_E = id(5);

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
    g(SAME_A, 'site-a', 35.68, 139.76),
    g(SAME_B, 'site-b', 35.68, 139.76),
    g(SAME_C, 'site-c', 35.68, 139.76),
    g(NEAR_D, 'site-d', 51.5, -0.12),
    g(NEAR_E, 'site-e', 51.5005, -0.12),
  ] as unknown as Json;
}

function summary(): Json {
  const none = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return {
    groups: {
      [SAME_A]: { ...none, ok: 4 },
      [SAME_B]: { ...none, critical: 1 },
      [NEAR_D]: { ...none, ok: 1 },
      [NEAR_E]: { ...none, warning: 2 },
    },
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

test('overlapping sites are one marker that lists them worst first', async ({ page, errors }) => {
  await page.goto('/topology/geo');
  const markers = page.getByTestId('geo-map-cluster');
  await expect(markers).toHaveCount(2);
  await expect(page.getByTestId('geo-map-pin')).toHaveCount(0);

  const same = markers.filter({ hasText: '3' });
  await expect(same).toHaveCount(1);
  await same.click();

  const list = page.getByRole('dialog', { name: '3 sites' });
  await expect(list).toBeVisible();
  const rows = list.locator('.geopage-bubble-row');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0)).toContainText(`${MOCK_PREFIX}site-b`);
  await expect(rows.nth(1)).toContainText(`${MOCK_PREFIX}site-a`);
  await expect(rows.nth(2)).toContainText('No nodes');
  // One coordinate: zooming cannot help, so it is not offered.
  await expect(list.getByRole('button', { name: 'Zoom in' })).toHaveCount(0);

  // A row hands over to that site's own bubble.
  await rows.nth(1).click();
  await expect(list).toHaveCount(0);
  await expect(page.getByRole('dialog', { name: `${MOCK_PREFIX}site-a` })).toBeVisible();

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test('sites that zooming can separate offer Zoom in', async ({ page, errors }) => {
  await page.goto('/topology/geo');
  const near = page.getByTestId('geo-map-cluster').filter({ hasText: '2' });
  await expect(near).toHaveCount(1);
  await near.click();

  const list = page.getByRole('dialog', { name: '2 sites' });
  await expect(list.locator('.geopage-bubble-row').first()).toContainText(`${MOCK_PREFIX}site-e`);
  await list.getByRole('button', { name: 'Zoom in' }).click();
  await expect(list).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});
