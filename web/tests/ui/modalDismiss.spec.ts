// SPDX-License-Identifier: AGPL-3.0-only
// The shared Modal — what closes it and what must not (ADR-052 Tier1).
//
// The defect: the backdrop closed the dialog on `click`, and the browser dispatches `click` on the
// nearest common ancestor of where the button went DOWN and where it came UP. Selecting text in a
// field and releasing a few pixels past the dialog's edge is therefore a click on the backdrop, so
// the dialog closed and took every field with it — a pasted CA certificate, a token.
//
// Only a browser can see this: it is about which element the engine names as a click's target,
// which jsdom does not model and Vitest never reaches (`Modal` is a `.tsx`).
//
// Both directions, because a dialog that never closes would pass the first test alone.

import { expect, test } from '../support/app';

async function openAddRule(page: import('@playwright/test').Page) {
  await page.goto('/alerts/rules');
  await page.getByRole('button', { name: /add/i }).first().click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  return dialog;
}

test('a drag that starts inside the dialog and ends on the backdrop does not close it', async ({
  page,
}) => {
  const dialog = await openAddRule(page);
  const box = await dialog.boundingBox();
  if (!box) throw new Error('the dialog has no box');

  // Down inside the dialog, up well outside it — the text-selection overshoot.
  await page.mouse.move(box.x + box.width / 2, box.y + 20);
  await page.mouse.down();
  await page.mouse.move(box.x - 40, box.y + 20, { steps: 4 });
  await page.mouse.up();

  await expect(dialog).toBeVisible();
});

test('a click that starts and ends on the backdrop still closes it', async ({ page }) => {
  const dialog = await openAddRule(page);
  const box = await dialog.boundingBox();
  if (!box) throw new Error('the dialog has no box');

  await page.mouse.click(Math.max(4, box.x - 40), box.y + 20);

  await expect(dialog).toBeHidden();
});
