// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Pollers after ADR-200 Inc.4: what the screen shows in place of the sentences it lost.
//
// 🚨 The first test is the one that matters. The create-pool dialog used to warn that a pool with
// nodes and no live poller has its jobs discarded silently. That sentence went only because the
// card now shows that state itself, as a pressable "No live poller" badge — so if the badge stops
// rendering, or stops opening its explanation, the warning is simply gone from the product.
//
// The route walk opens this screen with one healthy pool and never opens a dialog, so none of this
// is visible to it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

/** One healthy pool and one with nodes and no live poller, patched onto the generated body so the
 *  fixture follows `PollerListResponse` rather than transcribing it (ADR-052 decision 2). */
function fleet(): Json {
  const body = defaultBodyFor('/api/v1/pollers') as {
    pollers: Record<string, unknown>[];
    pools: Record<string, unknown>[];
  };
  const pool = body.pools[0];
  const poller = body.pollers[0];
  body.pools = [
    { ...pool, pool: 'default', nodes: 3, live_pollers: 1, warning: null, covered_by: null },
    {
      ...pool,
      pool: 'site-b',
      nodes: 12,
      live_pollers: 0,
      warning: 'nodes_without_live_poller',
      covered_by: null,
    },
  ];
  body.pollers = [
    { ...poller, id: 'poller-01', pool: 'default', status: 'online' },
    { ...poller, id: 'poller-02', pool: 'site-b', status: 'offline' },
  ];
  return body as unknown as Json;
}

test.use({
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/pollers': () => fleet() } },
});

test('a pool with nodes and no live poller says so on its card, and the badge explains', async ({
  page,
}) => {
  await page.goto('/settings/pollers');
  const warned = page.locator('.pool-card', { hasText: 'site-b' });
  const badge = warned.getByRole('button', { name: 'No live poller' });
  await expect(badge).toHaveCount(1);
  // The healthy pool carries no badge — a check that only ever finds one cannot tell a warning
  // from a decoration.
  await expect(page.locator('.pool-card', { hasText: 'default' }).locator('.infopress')).toHaveCount(0);

  await badge.click();
  const pop = page.locator('.infotip-pop');
  await expect(pop).toContainText('not being polled');
  await page.keyboard.press('Escape');
  await expect(pop).toHaveCount(0);
});

test('the funnel narrows the table to its pool, and the chip above the table releases it', async ({
  page,
}) => {
  await page.goto('/settings/pollers');
  const rows = page.locator('.dt-row');
  await expect(rows).toHaveCount(2);

  const funnel = page.getByRole('button', { name: 'Show only site-b pollers' });
  await funnel.click();
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText('poller-02');
  await expect(page.locator('.pool-card.is-selected')).toHaveCount(1);
  await expect(page.getByRole('button', { name: 'Clear this filter' }).first()).toHaveAttribute(
    'aria-pressed',
    'true',
  );

  const chip = page.locator('.pool-filter-chip', { hasText: 'site-b' });
  await expect(chip).toHaveCount(1);
  await chip.getByRole('button', { name: 'Clear this filter' }).click();
  await expect(rows).toHaveCount(2);
  await expect(page.locator('.pool-filter-chip')).toHaveCount(0);
});

test('the pool name says its rule only when it is broken', async ({ page }) => {
  await page.goto('/settings/pollers');
  await page.getByRole('button', { name: '+ Create pool' }).click();
  const modal = page.locator('.modal').last();
  const name = modal.getByLabel('Name', { exact: true });
  const error = modal.getByRole('alert');

  await expect(name).toHaveAttribute('placeholder', 'site-a');
  await expect(error).toHaveCount(0);

  await name.fill('site b!');
  await expect(error).toHaveText('Letters, digits, _ and - only (max 63)');
  await expect(modal.getByRole('button', { name: 'Create', exact: true })).toBeDisabled();

  // The accepting side in the same test: an error that never goes away passes the first half too.
  await name.fill('site-b2');
  await expect(error).toHaveCount(0);
  await expect(modal.getByRole('button', { name: 'Create', exact: true })).toBeEnabled();
});
