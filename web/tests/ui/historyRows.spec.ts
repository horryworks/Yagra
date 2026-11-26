// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ History since ADR-270: a row opens to show what that transition sent, and the delivery
// log is the screen's second view rather than a page of its own.

import { expect, test } from '../support/app';

test('a History row opens to the notifications that transition sent', async ({ page }) => {
  await page.goto('/alerts/history');
  const row = page.locator('.dt-row').first();
  await expect(row).toBeVisible();
  // Opening it asks the delivery log for this one alert, and only it.
  const asked = page.waitForRequest(
    (r) =>
      r.url().includes('/api/v1/notification-deliveries') &&
      r.url().includes('subject=') &&
      r.url().includes('check_id='),
  );
  await row.click();
  await asked;
  await expect(page.getByText('Notifications', { exact: true }).last()).toBeVisible();
  // A second click closes it again.
  await row.click();
  await expect(page.locator('.ui-facts')).toHaveCount(0);
});

test('the delivery log is History’s Notifications view, and its old address leads there', async ({ page }) => {
  await page.goto('/alerts/history');
  const log = page.waitForRequest((r) => r.url().includes('/api/v1/notification-deliveries'));
  await page.getByRole('tab', { name: 'Notifications' }).click();
  await log;
  await expect(page).toHaveURL(/\/alerts\/history\?view=deliveries$/);

  // A bookmark of the old page keeps its filter, under the key the view reads.
  await page.goto('/alerts/deliveries?result=failed');
  await expect(page).toHaveURL(/\/alerts\/history\?sent\.result=failed&view=deliveries/);
  await expect(page.getByRole('tab', { name: 'Notifications' })).toHaveAttribute('aria-selected', 'true');
});
