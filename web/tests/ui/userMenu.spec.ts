// SPDX-License-Identifier: AGPL-3.0-only
// The account badge menu and the Preferences dialog (ADR-055 決定 9 / Inc.7).
//
// Why Tier1 and not a unit test: there is no other kind available. `UserMenu` and
// `PreferencesModal` are `.tsx`, and Vitest runs `src/**/*.test.ts` in the node environment — a
// component test here is a file nothing executes (.claude/rules/testing.md). So the ordering the
// change was asked for, the dialog opening at all, and the legacy address still finding it are
// covered here or nowhere.
//
// The ordering assertion is deliberate and not cosmetic: "Preferences above Log out" is the
// request. A menu that renders both in either order looks correct in a screenshot review.

import { expect, test } from '../support/app';

/** Open the account badge menu and return its item labels, top to bottom. */
async function openMenu(page: import('@playwright/test').Page): Promise<string[]> {
  await page.locator('.usermenu-avatar').click();
  await expect(page.locator('.usermenu-pop')).toBeVisible();
  // `allTextContents()`, not `innerText`: text the browser has clipped is still in the DOM but is
  // absent from `innerText`, which has made a correct column look like a failing one before.
  return page.locator('.usermenu-item').allTextContents();
}

test('the menu lists Preferences above Log out', async ({ page }) => {
  await page.goto('/dashboard');
  expect(await openMenu(page)).toEqual(['Preferences', 'Log out']);
});

test('Preferences opens a dialog over the current screen, and the screen stays', async ({
  page,
}) => {
  await page.goto('/nodes');
  await openMenu(page);
  await page.getByRole('button', { name: 'Preferences', exact: true }).click();

  const dialog = page.locator('[role="dialog"]');
  await expect(dialog).toBeVisible();
  // Theme / language / layout — three exclusive choices, so three radiogroups.
  await expect(dialog.locator('[role="radiogroup"]')).toHaveCount(3);
  // The menu closed behind it, and the route did not move: the whole point of a dialog is that the
  // operator keeps the screen they were reading.
  await expect(page.locator('.usermenu-pop')).toHaveCount(0);
  expect(new URL(page.url()).pathname).toBe('/nodes');
});

test('choosing a theme applies it, and Escape closes the dialog', async ({ page }) => {
  await page.goto('/dashboard');
  // The harness seeds `theme: 'dark'`, so Light is the change that proves anything.
  expect(await page.getAttribute('html', 'data-theme')).toBe('dark');

  await openMenu(page);
  await page.getByRole('button', { name: 'Preferences', exact: true }).click();
  const dialog = page.locator('[role="dialog"]');
  await dialog.getByRole('radio', { name: 'Light' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  await expect(dialog.getByRole('radio', { name: 'Light' })).toHaveAttribute(
    'aria-checked',
    'true',
  );

  // Put it back before leaving: the pref persists to localStorage for the rest of this context.
  await dialog.getByRole('radio', { name: 'Dark' }).click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');

  await page.keyboard.press('Escape');
  await expect(dialog).toHaveCount(0);
  // Focus goes back to the badge, not to <body> — `Modal` would restore it to the menu item, which
  // unmounted with the menu.
  await expect(page.locator('.usermenu-avatar')).toBeFocused();
});

test('the address Preferences used to have opens the dialog instead of vanishing', async ({
  page,
}) => {
  await page.goto('/settings/preferences');
  await expect(page.locator('[role="dialog"]')).toBeVisible();
  expect(new URL(page.url()).pathname).toBe('/dashboard');
});

test('Settings no longer lists Preferences anywhere in its sidebar', async ({ page }) => {
  await page.goto('/settings/system-health');
  const items = await page.locator('.sidebar-item').allTextContents();
  expect(items.length).toBeGreaterThan(5);
  expect(items.map((s) => s.trim())).not.toContain('Preferences');
  // The group header went with it — it held that one item.
  expect(await page.locator('.sidebar-group-head').allTextContents()).toEqual(['System', 'Access']);
});
