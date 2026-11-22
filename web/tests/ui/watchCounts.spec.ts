// SPDX-License-Identifier: AGPL-3.0-only
// What a quiet screen is not seeing (ADR-227).
//
// Why Tier1: which chips are drawn and when the fleet counts as unpolled is decided in
// `lib/watchCounts.ts`, which Vitest runs. What only a browser proves is the wiring: the shell
// reading `/fleet/watch` on a screen that is not Active alerts, the sentence on Active alerts leading
// where the cause is (a sentence, not a chip, since ADR-227 Inc.2 — it counts nodes, not the rows it
// sits above), a fully watched fleet drawing neither, and "Clear all filters" counting the silenced
// chip as a filter it clears.

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
    const line = page.locator('.not-watched');
    await expect(line).toContainText('5 nodes are sending no data.');
    await expect(line.getByRole('link')).toHaveAttribute('href', '/settings/pollers');
    // It is not a filter on the list, so it is not drawn as one.
    await expect(page.locator('.watch-chip', { hasText: 'no data' })).toHaveCount(0);
  });
});

test.describe('a fleet that is fully watched', () => {
  test('draws neither sentence', async ({ page }) => {
    await page.goto('/alerts');
    await expect(page.locator('.dt-row, .dt-empty').first()).toBeVisible();
    await expect(page.locator('.nothing-polled')).toHaveCount(0);
    await expect(page.locator('.not-watched')).toHaveCount(0);
  });
});

test('narrowing to the silenced rows offers "Clear all filters", which takes it off', async ({
  page,
}) => {
  await page.goto('/alerts?silenced=1');
  const clear = page.getByRole('button', { name: /Clear all filters/ });
  await expect(clear).toHaveCount(1);
  await clear.click();
  await expect(page).not.toHaveURL(/silenced=1/);
});
