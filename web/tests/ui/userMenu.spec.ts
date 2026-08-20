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

// The three things the plan listed as "check by hand on the box". Two of them turned out to be
// Tier1's job after all: the dialog is the real bundle in a real browser either way, and the locale
// and the viewport are both client-side. Leaving them as homework would have been the easy wrong
// answer — a check nobody runs twice is not a check.

test('the dialog covers the viewport, though it is rendered inside the top bar', async ({
  page,
}) => {
  // 🚨 This is the app's first dialog opened from the shell chrome; every other one is opened from
  // a page. `Modal` does not `createPortal`, so the overlay lives inside `.topbar` — which is fine
  // only for as long as no ancestor there establishes a containing block (`transform`, `filter`) or
  // a stacking context (a positioned ancestor with a `z-index`). None does today. This test is what
  // says so tomorrow, and the failure it guards against is not subtle-looking: the dialog would sit
  // in the corner, or the top bar would paint over it.
  await page.goto('/nodes');
  await openMenu(page);
  await page.getByRole('button', { name: 'Preferences', exact: true }).click();

  const overlay = page.locator('.modal-overlay');
  await expect(overlay).toBeVisible();
  const box = (await overlay.boundingBox())!;
  const vp = page.viewportSize()!;
  expect(box.x).toBeLessThanOrEqual(1);
  expect(box.y).toBeLessThanOrEqual(1);
  expect(box.width).toBeGreaterThanOrEqual(vp.width - 1);
  expect(box.height).toBeGreaterThanOrEqual(vp.height - 1);

  // Geometry is half the question; paint order is the other half. Ask the browser what is actually
  // on top at a point over the top bar — the place a stacking-context bug would show first.
  const overTopBar = await page.evaluate(() =>
    document.elementFromPoint(window.innerWidth / 2, 20)?.className?.toString(),
  );
  expect(overTopBar).toContain('modal');
});

test('the dialog reads as Japanese in Japanese, with no raw keys', async ({ page }) => {
  // The strings moved namespace position and one was renamed (`prefs.pageNote` → `prefs.note`).
  // A key that resolves in neither locale passes the EN/JA parity gate — both are equally missing
  // it — and shows up only as `prefs.note` on screen.
  await page.goto('/dashboard');
  await openMenu(page);
  await page.getByRole('button', { name: 'Preferences', exact: true }).click();
  const dialog = page.locator('[role="dialog"]');
  await dialog.getByRole('radio', { name: '日本語' }).click();

  await expect(dialog.locator('.modal-title')).toHaveText('環境設定');
  const texts = await dialog
    .locator('.modal-title, .pref-note, .pref-name, .pref-help, .pref-seg-btn')
    .allTextContents();
  // Guard the guard: if the selectors stop matching, an empty list must not read as "no raw keys".
  expect(texts.length).toBeGreaterThanOrEqual(11);
  expect(texts.filter((s) => /^(prefs|nav|settings|common)\./.test(s.trim()))).toEqual([]);

  // The menu label is in the `nav` namespace, which is a separate bundle from the dialog's.
  await page.keyboard.press('Escape');
  expect(await openMenu(page)).toEqual(['環境設定', 'ログアウト']);
});

test.describe('on a phone', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('Layout ▸ Auto is reachable from the badge, so Desktop view is not a one-way door', async ({
    page,
  }) => {
    // 🚨 The drawer's "Desktop view" button sets `uiMode: 'desktop'` and offers no way back; the
    // only control that returns it to `auto` is in this dialog. Inc.7 removed Preferences from the
    // drawer, so if the badge could not reach the dialog on a phone, a tap would be permanent.
    // The harness seeds `uiMode: 'desktop'`, which is exactly the state to start from.
    await page.goto('/dashboard');
    await expect(page.locator('.topbar')).toBeVisible();

    await openMenu(page);
    await page.getByRole('button', { name: 'Preferences', exact: true }).click();
    await page.locator('[role="dialog"]').getByRole('radio', { name: 'Auto' }).click();

    // The shell flips under the dialog, and the mobile bar carries the same badge.
    await expect(page.locator('html')).toHaveAttribute('data-viewport', 'mobile');
    await page.keyboard.press('Escape');
    await expect(page.locator('.mtopbar .usermenu-avatar')).toBeVisible();

    // And the round trip closes: from the mobile bar, back to Desktop.
    expect(await openMenu(page)).toEqual(['Preferences', 'Log out']);
    await page.getByRole('button', { name: 'Preferences', exact: true }).click();
    await page.locator('[role="dialog"]').getByRole('radio', { name: 'Desktop' }).click();
    await expect(page.locator('html')).toHaveAttribute('data-viewport', 'desktop');
  });
});
