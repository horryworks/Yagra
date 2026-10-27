// SPDX-License-Identifier: AGPL-3.0-only
// The Geo map repeats east-west, and remembers where it was looking on the account (ADR-218).
//
// Why Tier1: the folding of the view, the pin's copy and the stored place are unit-tested
// (`geoProjection.test.ts`). What Vitest cannot run is the page drawing the second world where the
// first ends, a drag that keeps going past the date line, and a reload that opens where the
// operator left the map — the place arriving from `GET /api/v1/preferences` after the page has
// already fitted the pins.
//
// Every body is the generated one, patched (testing.md).

import type { Page } from '@playwright/test';
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

// The account document as the page last saved it. A reload re-seeds `localStorage`
// (`support/app.ts`), so a remembered place can only come back from the account. Reset per test —
// workers reuse this module.
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

type Box = { x: number; y: number; width: number; height: number };

async function box(page: Page, selector: string): Promise<Box> {
  const b = await page.locator(selector).first().boundingBox();
  expect(b, `${selector} has no box`).not.toBeNull();
  return b!;
}

/** Drag the map by `dx` pixels, starting in its left quarter so the press lands on the map, not
 *  on the site's pin (the fit centres the pane on it). */
async function dragBy(page: Page, dx: number): Promise<void> {
  const pane = await box(page, '.geopage-map');
  const x = pane.x + pane.width / 4;
  const y = pane.y + pane.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y, { steps: 8 });
  await page.mouse.up();
}

/** The pane is covered edge to edge by the world and the copy drawn after it. */
async function expectCovered(page: Page, why: string): Promise<void> {
  const pane = await box(page, '.geopage-map');
  const first = await box(page, '.geopage-ocean');
  const second = await box(page, '.geopage-svg use');
  expect(Math.abs(second.x - (first.x + first.width)), `${why}: the copy does not start where the world ends`).toBeLessThan(1);
  expect(first.x, `${why}: map shows empty space on the left`).toBeLessThanOrEqual(pane.x + 1);
  expect(second.x + second.width, `${why}: map shows empty space on the right`).toBeGreaterThanOrEqual(
    pane.x + pane.width - 1,
  );
}

async function pinX(page: Page): Promise<number> {
  const b = await box(page, '[data-testid="geo-map-pin"]');
  return b.x + b.width / 2;
}

test('a drag keeps going past the date line, and one world later the site is back', async ({
  page,
  errors,
}) => {
  await page.goto('/topology/geo');
  const pins = page.getByTestId('geo-map-pin');
  await expect(pins).toHaveCount(1);
  await expectCovered(page, 'as fitted');
  const before = await pinX(page);
  const world = (await box(page, '.geopage-ocean')).width;

  for (let i = 1; i <= 4; i++) {
    await dragBy(page, world / 4);
    await expectCovered(page, `after ${i} quarter${i > 1 ? 's' : ''} of a world`);
    await expect(pins, 'a site is drawn once, whichever copy of the world shows it').toHaveCount(1);
  }
  expect(Math.abs((await pinX(page)) - before), 'one world later the site is not where it was').toBeLessThan(2);
  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test('a reload opens the map where it was left, from the account', async ({ page, errors }) => {
  await page.goto('/topology/geo');
  await expect(page.getByTestId('geo-map-pin')).toHaveCount(1);

  const saved = page.waitForRequest(
    (r) =>
      r.method() === 'PUT' &&
      new URL(r.url()).pathname === '/api/v1/preferences' &&
      (r.postDataJSON() as { geoMapPlace?: unknown } | null)?.geoMapPlace !== undefined,
  );
  const fitted = await pinX(page);
  await dragBy(page, 180);
  await saved;
  const left = await pinX(page);
  expect(Math.abs(left - fitted)).toBeGreaterThan(100);

  // The account's answer is held back until the map has been drawn, so the page fits the pins
  // first and has to move to the remembered place when it arrives — the order a sign-in moments
  // before opening the page produces. Without the hold the mock answers before the pane is
  // measured, and the late path is never taken (measured: breaking it left this test green).
  let release: () => void = () => undefined;
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route('**/api/v1/preferences', async (route) => {
    if (route.request().method() === 'GET') await held;
    await route.fallback();
  });
  const adopted = page.waitForResponse(
    (r) => r.request().method() === 'GET' && new URL(r.url()).pathname === '/api/v1/preferences',
  );
  await page.goto(page.url());
  await expect(page.getByTestId('geo-map-pin')).toHaveCount(1);
  expect(Math.abs((await pinX(page)) - fitted), 'before the account answers, the pins are fitted').toBeLessThan(2);
  release();
  await adopted;
  await expect
    .poll(() => pinX(page), { message: 'the reload did not open where the map was left' })
    .toBeCloseTo(left, 0);
  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});
