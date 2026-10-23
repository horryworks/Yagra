// SPDX-License-Identifier: AGPL-3.0-only
// A screen whose code-split chunk cannot be fetched (ADR-212).
//
// The case: a browser still on the page from before an upgrade moves to a lazily loaded section,
// asks for a hashed chunk the new image no longer has, and gets nginx's 404. Before ADR-212 the
// rejected import escaped React and unmounted the whole app — a blank page, shell included. Only a
// browser can show that: it is about what React does with a failed dynamic import.
//
// Every script request 404s from the moment the dashboard has loaded, the way it would after an
// upgrade. The dashboard itself is in the initial chunk, so it is already on screen.

import { expect, test } from '../support/app';

async function failChunksThenOpenSettings(page: import('@playwright/test').Page) {
  await page.goto('/dashboard');
  await expect(page.getByRole('navigation').first()).toBeVisible();
  await page.route('**/assets/*.js', (r) => r.fulfill({ status: 404, body: 'gone' }));
  // A client-side move, as a click in the sidebar would make: no new document, so the page keeps
  // the old index.html and only the Settings group's chunk is asked for.
  await page.evaluate(() => {
    history.pushState({}, '', '/settings/about');
    window.dispatchEvent(new PopStateEvent('popstate'));
  });
}

test('a chunk that cannot be fetched shows a reload button, not a blank page', async ({
  page,
  errors,
}) => {
  await failChunksThenOpenSettings(page);

  await expect(page.getByText('This screen could not be loaded.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Reload page' })).toBeVisible();
  // The shell stays: the operator can still go somewhere else.
  await expect(page.getByRole('navigation').first()).toBeVisible();
  expect(errors.uncaught).toEqual([]);
});

test('the reload button loads the screen once the chunk is there again', async ({ page }) => {
  await failChunksThenOpenSettings(page);
  const reload = page.getByRole('button', { name: 'Reload page' });
  await expect(reload).toBeVisible();

  await page.unroute('**/assets/*.js');
  await reload.click();

  await expect(page).toHaveURL(/\/settings\/about$/);
  await expect(page.getByText('This screen could not be loaded.')).toHaveCount(0);
  await expect(page.locator('.pageheader-note')).toBeVisible();
});

test('moving to a screen in the initial chunk clears the failure', async ({ page }) => {
  await failChunksThenOpenSettings(page);
  await expect(page.getByRole('button', { name: 'Reload page' })).toBeVisible();

  await page.evaluate(() => {
    history.pushState({}, '', '/alerts');
    window.dispatchEvent(new PopStateEvent('popstate'));
  });
  await expect(page.getByText('This screen could not be loaded.')).toHaveCount(0);
  await expect(page.locator('.pageheader-note')).toBeVisible();
});
