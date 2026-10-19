// SPDX-License-Identifier: AGPL-3.0-only
// A Geo map pin opens a bubble: the site's name, the nodes under it with an active alert, and the
// two places to go next (ADR-191 Inc.15).
//
// Why Tier1: which alerts belong to which pin is unit-tested (`geoPinAlerts.test.ts`). What Vitest
// cannot run is the page reading the alert store, asking `node-names` for the alerting nodes, and
// drawing the rows — nor the bubble fitting the screen. The walk cannot stand in: it never presses
// a pin, and the generated mock has no alert under one.
//
// Every body is the generated one, patched — a hand-written one would be a second copy of the
// contract (testing.md).

import type { Request } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const SITE = id(1); // placed, carries the alerts
const FLOOR = id(2); // no coordinates of its own: inherits SITE's pin
const QUIET = id(3); // placed, no alerts
const ALERTING = 7; // one more than the bubble lists, plus one
const node = (i: number) => id(1000 + i);

function folders(): Json {
  const [t] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const g = (gid: string, name: string, geo: string, lat: number | null, lon: number | null) => ({
    ...t,
    id: gid,
    name: `${MOCK_PREFIX}${name}`,
    parent_id: gid === FLOOR ? SITE : null,
    origin: null,
    latitude: lat,
    longitude: lon,
    geo_group: geo,
  });
  return [
    g(SITE, 'SITE-01 Tokyo', SITE, 35.68, 139.76),
    g(FLOOR, 'floor-3', SITE, null, null),
    g(QUIET, 'SITE-07 Osaka', QUIET, 34.69, 135.5),
  ] as unknown as Json;
}

/** Seven alerting nodes: the first two filed in the site, the rest on its floor, plus one more
 *  alert on node 0 so it carries two. Critical ones are older, to prove the order is by severity. */
function alerts(): Json {
  const [t] = defaultBodyFor('/api/v1/alerts') as Record<string, Json>[];
  const a = (i: number, severity: string, minutesAgo: number, metric: string) => ({
    ...t,
    node: node(i),
    subject_kind: 'node',
    subject_name: null,
    severity,
    metric,
    breach: null,
    ifindex: null,
    row: null,
    at_unix_ms: Date.now() - minutesAgo * 60_000,
  });
  const out = [];
  for (let i = 0; i < ALERTING; i++) out.push(a(i, i < 2 ? 'critical' : 'warning', 100 + i, `m${i}`));
  out.push(a(0, 'warning', 5, 'second'));
  return out as unknown as Json;
}

function names(_url: URL, request: Request): Json {
  const ids = (request.postDataJSON() as { ids: string[] }).ids;
  return ids.map((nid) => {
    const i = Number(nid.slice(-4)) - 1000;
    return { id: nid, name: `${MOCK_PREFIX}sw-${i}`, group_id: i < 2 ? SITE : FLOOR };
  }) as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': folders(),
      '/api/v1/alerts': alerts(),
      '/api/v1/node-names': names,
    },
  },
});

test('a pin with alerting nodes lists them worst first and opens the node list', async ({
  page,
  errors,
}) => {
  await page.goto('/topology/geo');
  const pins = page.getByTestId('geo-map-pin');
  await expect(pins).toHaveCount(2);
  await pins.first().click();

  const bubble = page.getByRole('dialog', { name: `${MOCK_PREFIX}SITE-01 Tokyo` });
  await expect(bubble).toBeVisible();
  const rows = bubble.locator('.geopage-bubble-row');
  await expect(rows).toHaveCount(5);
  await expect(rows.first()).toContainText(`${MOCK_PREFIX}sw-0`);
  await expect(rows.nth(1)).toContainText(`${MOCK_PREFIX}sw-1`);
  await expect(bubble).toContainText(`${ALERTING} with alerts`);
  await expect(bubble).toContainText(`+${ALERTING - 5} more`);

  // Inside the window, whatever the pin's position.
  const box = (await bubble.boundingBox())!;
  const vp = page.viewportSize()!;
  expect(box.x).toBeGreaterThanOrEqual(0);
  expect(box.x + box.width).toBeLessThanOrEqual(vp.width);

  await bubble.getByRole('button', { name: 'Open in the node list' }).click();
  await expect(page).toHaveURL(new RegExp(`/nodes\\?sel=group%3A${SITE}`));

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test('a pin with no alerting node says so, and a row opens the node', async ({ page, errors }) => {
  await page.goto('/topology/geo');
  const pins = page.getByTestId('geo-map-pin');
  await expect(pins).toHaveCount(2);

  await pins.nth(1).click();
  const quiet = page.getByRole('dialog', { name: `${MOCK_PREFIX}SITE-07 Osaka` });
  await expect(quiet).toContainText('No active alerts');
  await expect(quiet.locator('.geopage-bubble-row')).toHaveCount(0);
  await page.keyboard.press('Escape');
  await expect(quiet).toHaveCount(0);

  await pins.first().click();
  const busy = page.getByRole('dialog', { name: `${MOCK_PREFIX}SITE-01 Tokyo` });
  await busy.locator('.geopage-bubble-row').first().click();
  await expect(page).toHaveURL(new RegExp(`/nodes/${node(0)}$`));

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});
