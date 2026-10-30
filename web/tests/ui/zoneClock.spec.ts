// SPDX-License-Identifier: AGPL-3.0-only
// The top-bar clock and the display zone (ADR-221).
//
// Why Tier1: `ZoneClock` and the page remount it causes are `.tsx`, which Vitest never runs. The
// arithmetic is pinned in `lib/timeZone.test.ts`; what only a browser can show is that picking a
// zone redraws the times already on screen, and that "follow the browser" undoes it.

import { expect, test } from '../support/app';
import { TEST_IDS } from '../../src/testIds';

// Fixed, so the "before" and "after" times differ on every machine that runs this.
test.use({ timezoneId: 'Asia/Tokyo' });

test('picking a zone redraws the clock and the times on the page, and following the browser undoes it', async ({
  page,
}) => {
  // The generated mock's `at` is not a date. Serve one row whose time reads differently in Tokyo
  // and in New York, so the assertion below is about the zone and not about the fixture.
  await page.route('**/api/v1/audit?*', (route) =>
    route.request().method() === 'GET'
      ? route.fulfill({
          json: [
            {
              id: '00000000-0000-4000-8000-000000000001',
              at: '2026-10-30T03:04:05Z',
              username: 'admin',
              action: 'GET /api/v1/nodes',
              status: 200,
            },
          ],
        })
      : route.fallback(),
  );
  await page.goto('/settings/audit');
  const clock = page.getByRole('button', { name: 'Time zone', exact: true });
  const dot = page.getByTestId(TEST_IDS.zoneClockDot);
  const firstTime = page.locator('.yt-time-exact').first();

  await expect(clock).toContainText('GMT+9');
  await expect(dot).toHaveCount(0);
  const before = await firstTime.textContent();

  await clock.click();
  const picker = page.getByRole('dialog', { name: 'Time zone' });
  await picker.getByRole('searchbox', { name: 'Time zone' }).fill('new york');
  await picker.getByRole('button', { name: /^America\/New_York/ }).click();

  await expect(clock).toContainText(/E[SD]T/);
  await expect(dot).toHaveCount(1);
  await expect(firstTime).not.toHaveText(before ?? '');

  await clock.click();
  await page
    .getByRole('dialog', { name: 'Time zone' })
    .getByRole('button', { name: /^Follow browser/ })
    .click();

  await expect(clock).toContainText('GMT+9');
  await expect(dot).toHaveCount(0);
  await expect(firstTime).toHaveText(before ?? '');
});
