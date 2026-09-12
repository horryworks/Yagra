// SPDX-License-Identifier: AGPL-3.0-only
// The top-bar tab returns to the screen the operator left in that section (ADR-134 増分 2).
//
// Why Tier1 and not a unit test: the decision itself is pure and tested in `src/nav.test.ts`
// (`sectionLandingPath` / `rememberableRoute`). What cannot be tested there is the loop this
// feature *is* — one component records the route, another reads the record back, and a router
// carries the operator between them. `TopBar.tsx` and `AppShell.tsx` are `.tsx`, and Vitest runs
// `src/**/*.test.ts` in the node environment (.claude/rules/testing.md), so the round trip is
// covered here or nowhere.
//
// sessionStorage needs no clearing between tests: each test gets its own browser context, and
// `tests/support/app.ts`'s `addInitScript` writes only the two localStorage keys.

import { expect, test } from '../support/app';

/** A section tab in the top bar.
 *
 * 🚨 Scoped to `.topbar-tabs` deliberately. The Events section's label and its first item's label
 * are both "Events", so a bare `getByRole('link', { name: 'Events' })` matches the tab *and* the
 * sidebar item and fails on strict mode — the same trap `interfaceDock.spec.ts` hit with nine
 * column grips answering to `role="slider"`. */
function tab(page: import('@playwright/test').Page, name: string) {
  return page.locator('.topbar-tabs').getByRole('link', { name, exact: true });
}

/** The sub-page the sidebar is showing as current, in the menu's own words. This is the thing the
 *  report was about — "Dashboard に戻ってくると Shared dashboard が見える" — so asserting the
 *  screen rather than only the URL is what makes the test about the complaint. */
function activeItem(page: import('@playwright/test').Page) {
  return page.locator('.sidebar-item.active .sidebar-label');
}

test('the Dashboard tab returns to the board the operator left, not to Shared dashboard', async ({
  page,
}) => {
  await page.goto('/dashboard/my');
  await expect(activeItem(page)).toHaveText('My dashboard');

  // Step away into another section and come back the way the operator did: by pressing the tab.
  await tab(page, 'Nodes').click();
  await expect(page).toHaveURL(/\/nodes$/);
  await expect(activeItem(page)).toHaveText('All nodes');

  await tab(page, 'Dashboard').click();
  await expect(page).toHaveURL(/\/dashboard\/my$/);
  // Before 増分 2 this read "Shared dashboard": the tab's `to=` was a constant.
  await expect(activeItem(page)).toHaveText('My dashboard');
});

test('a narrowed list comes back narrowed, and says so', async ({ page }) => {
  // 決定 9 carries the query string, and what makes that safe is on screen rather than in the
  // code: the filter row cannot be closed while it filters, and this button counts what is
  // narrowing the list (ADR-053 Inc.9). If the reset control ever stops being drawn, the decision
  // to remember filters has lost its justification — which is why it is asserted here and not
  // merely described in the ADR.
  const reset = page.getByRole('button', { name: /Clear all filters/ });

  await page.goto('/events?message=router');
  await expect(reset).toBeVisible();

  await tab(page, 'Nodes').click();
  await expect(page).toHaveURL(/\/nodes$/);

  await tab(page, 'Events').click();
  await expect(page).toHaveURL(/\/events\?message=router$/);
  await expect(reset).toBeVisible();
});

test('the logo stays home while the tabs remember', async ({ page }) => {
  // 決定 10. The memory for the dashboard section says `/dashboard/my` by the time the logo is
  // pressed, so a home button that followed the memory would land there — this asserts it does
  // not. One control with a predictable destination is the way out of a memory that surprises.
  await page.goto('/dashboard/my');
  await expect(activeItem(page)).toHaveText('My dashboard');

  await tab(page, 'Nodes').click();
  await expect(page).toHaveURL(/\/nodes$/);

  await page.locator('.topbar-home').click();
  await expect(page).toHaveURL(/\/dashboard$/);
  await expect(activeItem(page)).toHaveText('Shared dashboard');
});
