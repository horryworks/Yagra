// SPDX-License-Identifier: AGPL-3.0-only
// ADR-228 Inc.2: a list held in the browser sorts by any column that can say what a row is worth,
// from the first press of its header, and keeps the order in the URL beside its filters.
//
// The values themselves are `lib/clientSort.ts`'s and tested there. What only a browser shows is
// the wiring: that the header became a button, that the press reached the URL, and that the rows
// moved.

import { expect, test } from '../support/app';

test('Pollers opens in arrival order and sorts from the first press of a header', async ({ page }) => {
  await page.goto('/settings/pollers');
  const header = page.locator('.dt-head .dt-h-sort', { hasText: 'Poller' });
  await expect(header).toBeVisible();
  // Arrival order: no column is marked until the operator chooses one.
  await expect(page.locator('.dt-head .dt-h-sort.active')).toHaveCount(0);

  await header.click();
  await expect(page).toHaveURL(/[?&]sort=poller&dir=asc/);
  await expect(header).toHaveAttribute('aria-sort', 'ascending');

  await header.click();
  await expect(page).toHaveURL(/[?&]dir=desc/);
  await expect(header).toHaveAttribute('aria-sort', 'descending');

  // A reload keeps it: the order is in the URL, not in component state (ADR-153).
  await page.goto(page.url());
  await expect(page.locator('.dt-head .dt-h-sort', { hasText: 'Poller' })).toHaveAttribute(
    'aria-sort',
    'descending',
  );
});

test('every data column of the Pollers list can sort; only the actions cannot', async ({ page }) => {
  await page.goto('/settings/pollers');
  await expect(page.locator('.dt-head .dt-h').first()).toBeVisible();
  const unsortable = await page.evaluate(() =>
    Array.from(document.querySelectorAll('.dt-head > .dt-h'))
      .filter((c) => !c.querySelector('.dt-h-sort'))
      .map((c) => (c.textContent || '').trim()),
  );
  expect(unsortable).toEqual(['Actions']);
});
