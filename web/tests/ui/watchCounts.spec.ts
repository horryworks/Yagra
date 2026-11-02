// SPDX-License-Identifier: AGPL-3.0-only
// What a quiet screen is not seeing (ADR-227).
//
// Why Tier1: which chips are drawn and when the fleet counts as unpolled is decided in
// `lib/watchCounts.ts`, which Vitest runs. What only a browser proves is the wiring: the shell
// reading `/fleet/watch` on a screen that is not Active alerts, the chip on Active alerts leading
// where the cause is, and a fully watched fleet drawing neither.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

const watching = (w: {
  no_live_poller: number;
  no_recent_data: number;
  pool_polled: number;
}) => ({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/fleet/watch': { ...w, pollers_known: true } as unknown as Json,
    },
  },
});

test.describe('a fleet whose only pool has no live poller', () => {
  test.use(watching({ no_live_poller: 3, no_recent_data: 2, pool_polled: 3 }));

  test('the shell says so on a screen that is not about alerts, with the way to the pollers', async ({
    page,
  }) => {
    await page.goto('/dashboard');
    const line = page.locator('.nothing-polled');
    await expect(line).toBeVisible();
    await expect(line.getByRole('link')).toHaveAttribute('href', '/settings/pollers');
  });

  test('Active alerts counts both reasons together and leads to the pollers', async ({ page }) => {
    await page.goto('/alerts');
    const chip = page.locator('.watch-chip', { hasText: 'Not watched 5' });
    await expect(chip).toBeVisible();
    await expect(chip).toHaveAttribute('href', '/settings/pollers');
  });
});

test.describe('a fleet that is fully watched', () => {
  test('draws neither the line nor the chip', async ({ page }) => {
    await page.goto('/alerts');
    await expect(page.locator('.alertrows-empty, .alertrow').first()).toBeVisible();
    await expect(page.locator('.nothing-polled')).toHaveCount(0);
    await expect(page.locator('.watch-chip', { hasText: 'Not watched' })).toHaveCount(0);
  });
});
