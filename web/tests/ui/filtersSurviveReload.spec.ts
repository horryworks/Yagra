// SPDX-License-Identifier: AGPL-3.0-only
// A filter an operator set is still set after a reload (ADR-153).
//
// Why Tier1: the codec and the hooks are unit-tested, but which key each screen passes — and whether
// a route with two or three tables gave each one its own prefix — is decided in `.tsx` files Vitest
// never runs. Each test here sets a filter through the real control, reloads from the URL the screen
// itself wrote (`page.goto(page.url())` — the whole of what an F5 keeps, since the harness re-seeds
// localStorage on every navigation), and checks the control still says so.
//
// The screens here are the ones that USED to lose their filters. The ones that were already
// URL-backed have their own round-trip tests (`columnFilter.spec.ts` for the Events log).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

type Page = import('@playwright/test').Page;
type Locator = import('@playwright/test').Locator;

test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

const NEEDLE = 'edge';

/** Open a text column's filter under `scope`, type into it and commit.
 *
 *  Enter commits at once; the box otherwise commits on its settle, and closing the popover before
 *  that would unmount the draft with the term uncommitted — a test artefact, not a screen bug. */
async function typeFilter(page: Page, scope: Locator | Page, column: string, term: string) {
  await scope.getByRole('button', { name: `Filter by ${column}` }).click();
  const box = page.getByRole('dialog').getByRole('searchbox').first();
  await box.fill(term);
  await box.press('Enter');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
}

const reload = async (page: Page) => {
  await page.goto(page.url());
};

test.describe('Reports — three tables and a tab on one route', () => {
  test('the tab and its table filter survive a reload, under that table’s own key', async ({ page }) => {
    await page.goto('/dashboard/reports');
    await page.getByRole('tab', { name: /Schedules/ }).click();
    await expect(page, 'the tab never reached the URL').toHaveURL(/[?&]tab=schedules/);

    await typeFilter(page, page, 'Report', NEEDLE);
    await expect(page, 'the filter was not written under the schedules prefix').toHaveURL(
      new RegExp(`[?&]schedules\\.name=[^&]*${NEEDLE}`),
    );
    expect(new URL(page.url()).searchParams.has('name'), 'a bare `name` key was written').toBe(false);

    await reload(page);
    await expect(page.getByRole('tab', { name: /Schedules/ }), 'the reload fell back to the first tab').toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(page.getByRole('button', { name: /Clear all filters/ })).toHaveCount(1);

    // The same column on another tab is a different table: it must not arrive filtered.
    await page.getByRole('tab', { name: /Saved reports/ }).click();
    await expect(page.getByRole('button', { name: /Clear all filters/ })).toHaveCount(0);
  });
});

test.describe('Notification delivery — two tables with the same column keys', () => {
  const section = (page: Page, title: string) =>
    page.locator('section').filter({ has: page.getByRole('heading', { name: title }) });

  test('a channels filter survives a reload and does not narrow the rules table', async ({ page }) => {
    await page.goto('/alerts/routing');
    const channels = section(page, 'Notification channels');
    const rules = section(page, 'Routing rules');
    await expect(channels).toHaveCount(1);
    await expect(rules).toHaveCount(1);

    await typeFilter(page, channels, 'Name', NEEDLE);
    await expect(page).toHaveURL(new RegExp(`[?&]channels\\.name=[^&]*${NEEDLE}`));
    await expect(channels.getByRole('button', { name: /Clear all filters/ })).toHaveCount(1);
    await expect(rules.getByRole('button', { name: /Clear all filters/ }), 'the rules table picked up the channels filter').toHaveCount(0);

    await reload(page);
    await expect(channels.getByRole('button', { name: /Clear all filters/ }), 'the reload dropped the filter').toHaveCount(1);
    await expect(rules.getByRole('button', { name: /Clear all filters/ })).toHaveCount(0);

    await channels.getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page).not.toHaveURL(/channels\.name=/);
  });
});
