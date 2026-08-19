// SPDX-License-Identifier: AGPL-3.0-only
// Clearing the inventory selection (ADR-073) — three gestures, and the two presses that must NOT
// clear it.
//
// Why Tier1 and not a unit test: the judgement itself (`lib/escapeDismiss.ts`) is unit-tested, but
// nothing there can say whether the key press reaches the page, whether a click on the tree's blank
// space is a click on the tree, or whether the modal above it swallowed the Escape first. Those are
// wiring and layout, which is what a browser is for.
//
// The layering assertions are the load-bearing half. "Escape clears the selection" is easy to make
// true and easy to make true too often — closing a dialog and throwing away the row behind it in
// one press is a worse bug than the one this ADR fixes.

import { expect, test } from '../support/app';

/** The selection lives in `?sel=`; `URLSearchParams` percent-encodes the colon, so match the key. */
const selected = (page: { url(): string }) => new URL(page.url()).searchParams.get('sel');

/** Two animation frames — long enough for a document-level handler to have undone the click. */
async function settle(page: import('@playwright/test').Page) {
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(null)))),
  );
}

test('a group row selects, and Escape clears it', async ({ page }) => {
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();

  await row.click();
  await expect(page.locator('.ntree-row.sel')).toHaveCount(1);
  expect(selected(page), 'clicking a row wrote no selection').toMatch(/^group:/);

  await page.keyboard.press('Escape');
  await expect(page.locator('.ntree-row.sel')).toHaveCount(0);
  expect(selected(page)).toBeNull();

  // The selection is URL state and the effect re-runs on it, so a re-select loop would show up as
  // the parameter coming back rather than as a visible flicker.
  await settle(page);
  expect(selected(page), 'the selection came back after Escape').toBeNull();
});

test('clicking the selected row again clears it', async ({ page }) => {
  // The gesture that works when the other two cannot: it needs neither a keyboard nor blank space.
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();

  await row.click();
  expect(selected(page)).toMatch(/^group:/);

  await row.click();
  await settle(page);
  expect(selected(page), 'a second click on the same row did not clear it').toBeNull();
  await expect(page.locator('.ntree-row.sel')).toHaveCount(0);
});

test('clicking the empty space below the rows clears the selection', async ({ page }) => {
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();
  await row.click();
  expect(selected(page)).toMatch(/^group:/);

  // Aim at the bottom of the scroller, and assert first that the point is actually blank —
  // otherwise a tree that grew past the fold would turn this into "clicked a row" and the test
  // would pass for the wrong reason (or fail confusingly).
  const scroller = page.locator('.ntree-body');
  const box = await scroller.boundingBox();
  expect(box, 'the tree body has no box').not.toBeNull();
  const point = { x: 12, y: box!.height - 8 };
  const onBlank = await scroller.evaluate(
    (el, p) =>
      document.elementFromPoint(el.getBoundingClientRect().left + p.x, el.getBoundingClientRect().top + p.y) === el,
    point,
  );
  expect(onBlank, 'the bottom of the tree is covered by rows — pick a different point').toBe(true);

  await scroller.click({ position: point });
  await settle(page);
  expect(selected(page), 'clicking blank tree space did not clear the selection').toBeNull();
});

test('Escape closes the row menu without touching the selection', async ({ page }) => {
  // The layering rule: a transient surface takes the press, and what it sits above is left alone.
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();
  await row.click();
  const before = selected(page);
  expect(before).toMatch(/^group:/);

  await row.hover();
  await row.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByRole('menu')).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(page.getByRole('menu')).toHaveCount(0);
  await settle(page);
  expect(selected(page), 'Escape closed the menu AND cleared the selection').toBe(before);
});

test('Escape closes a dialog without touching the selection', async ({ page }) => {
  await page.goto('/nodes');
  const row = page.locator('.ntree-grow').first();
  await expect(row).toBeVisible();
  await row.click();
  const before = selected(page);
  expect(before).toMatch(/^group:/);

  await row.hover();
  await row.getByRole('button', { name: 'Add' }).click();
  await page.getByRole('menuitem').first().click();
  await expect(page.getByRole('dialog')).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
  await settle(page);
  expect(selected(page), 'Escape closed the dialog AND cleared the selection').toBe(before);
});
