// SPDX-License-Identifier: AGPL-3.0-only
// A dashboard whose document could not be read (ADR-052 Tier1).
//
// The defect: a failed `GET /dashboard` adopted the five-widget default with status 'error', and no
// page read that status — so the default was drawn as the operator's own board with Customize live
// beside it, and the first edit saved it over every board they had built.
//
// `layoutStore.test.ts` pins the store half (nothing adopted, nothing saved). What only a browser
// can see is the half an operator acts on: that no edit control is DRAWN, and that a retry is.
//
// Both directions: the healthy board must still offer Customize, or "never draw it" would pass.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

test.describe('when the board cannot be read', () => {
  test.use({
    mockConfig: { overrides: BOOTSTRAP_OVERRIDES, failures: { '/api/v1/dashboard': 500 } },
  });

  test('My Dashboard offers a retry and no way to edit', async ({ page, mock }) => {
    await page.goto('/dashboard/my');
    await expect(page.getByText(/could not be loaded/i)).toBeVisible();
    await expect(page.getByRole('button', { name: /retry/i })).toBeVisible();
    await expect(page.getByRole('button', { name: /customize/i })).toHaveCount(0);
    // …and nothing was written, which is the point of all of it.
    expect(mock.requests.filter((r) => r.method !== 'GET' && r.pathname === '/api/v1/dashboard')).toEqual(
      [],
    );
  });
});

test('a board that loads still offers Customize', async ({ page }) => {
  await page.goto('/dashboard/my');
  await expect(page.getByRole('button', { name: /customize/i })).toBeVisible();
  await expect(page.getByText(/could not be loaded/i)).toHaveCount(0);
});
