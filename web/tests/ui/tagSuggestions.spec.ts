// SPDX-License-Identifier: AGPL-3.0-only
// The tag box lists the registered tags as soon as it is clicked (ADR-263 Inc.3).
//
// Inc.2 wired the registered tags to the box as a native `datalist`. On a lab deployment the list
// was attached (`input.list.options` held the tag) and the operator still saw nothing: Chromium
// shows a datalist only once a match is typed, and a pick from it is not a chip until Enter. A
// native popup is not in the DOM, so only a browser test of the list we draw ourselves can pin this.
// The mock registers `APAC` and `TeamC` (`tests/support/bootstrap.ts`).

import { expect, test } from '../support/app';
import { TREE_SIBLING_IDS } from '../support/bootstrap';

type Page = import('@playwright/test').Page;

async function openTags(page: Page) {
  await page.goto(`/nodes?sel=node:${TREE_SIBLING_IDS[0]}`);
  await page.locator('.nodes-detail-pane .nd-head').getByRole('button', { name: 'Edit node' }).click();
  const box = page.getByRole('dialog').getByRole('combobox', { name: 'Tags' });
  await box.click();
  return box;
}

test('clicking the tag box lists the registered tags, and a click on one makes it a chip', async ({
  page,
}) => {
  await openTags(page);
  const list = page.getByRole('listbox', { name: 'Registered tags' });
  await expect(list.getByRole('option')).toHaveText(['APAC', 'TeamC']);

  await list.getByRole('option', { name: 'TeamC' }).click();
  await expect(page.locator('.chipinput-chip', { hasText: 'TeamC' })).toBeVisible();
  // The list stays open with what is left, so a second tag is one more click.
  await expect(list.getByRole('option')).toHaveText(['APAC']);
});

test('typing narrows the list, and the arrows and Enter pick from it', async ({ page }) => {
  const box = await openTags(page);
  await box.fill('ap');
  const list = page.getByRole('listbox', { name: 'Registered tags' });
  await expect(list.getByRole('option')).toHaveText(['APAC']);
  await box.press('ArrowDown');
  await box.press('Enter');
  await expect(page.locator('.chipinput-chip', { hasText: 'APAC' })).toBeVisible();
  await expect(box).toHaveValue('');
});

test('Escape closes the list and leaves the dialog open', async ({ page }) => {
  const box = await openTags(page);
  await expect(page.getByRole('listbox', { name: 'Registered tags' })).toBeVisible();
  await box.press('Escape');
  await expect(page.getByRole('listbox', { name: 'Registered tags' })).toHaveCount(0);
  await expect(page.getByRole('dialog')).toBeVisible();
});
