// SPDX-License-Identifier: AGPL-3.0-only
// The ⓘ inside a dialog, used rather than looked at (ADR-200).
//
// `InfoTip` is a popover with `role="dialog"` opened from inside a `Modal`, which is the case two
// shared pieces have to get right together: Escape must close the popover and leave the form, and
// the dialog's Tab trap must keep trapping while the popover is open. The trap used to take "the
// last `[role="dialog"]` in the document" as the frontmost dialog — and a portalled popover is
// exactly that, so the trap switched itself off. Only a browser can see either: Vitest never runs a
// `.tsx`, and both are about which listener sees a key first.
//
// The subject is Settings ▸ API tokens ▸ New token ▸ Owner, the first field to carry an ⓘ.
// Visibility is read from `getComputedStyle`, never `isVisible()` (`testing.md`).

import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../support/app';

async function openOwnerTip(page: Page): Promise<{ dialog: Locator; tip: Locator }> {
  await page.goto('/settings/api-tokens');
  await page.getByRole('button', { name: /new token/i }).first().click();
  const dialog = page.getByRole('dialog', { name: 'Create API token' });
  await expect(dialog).toBeVisible();
  const tip = dialog.getByRole('button', { name: 'About Owner' });
  await expect(tip).toHaveCount(1);
  return { dialog, tip };
}

/** Whether the explanation is on screen: in the document, and not the measuring frame's
 *  `visibility: hidden`. */
async function popoverShown(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const el = document.querySelector<HTMLElement>('.infotip-pop');
    if (!el) return false;
    const cs = getComputedStyle(el);
    return cs.visibility !== 'hidden' && cs.display !== 'none' && Number(cs.opacity) > 0;
  });
}

const OWNER_TEXT = /A token acts as its owner/;

test('pressing the ⓘ opens the explanation, and pressing it again closes it', async ({ page }) => {
  const { tip } = await openOwnerTip(page);
  await tip.click();
  await expect.poll(() => popoverShown(page)).toBe(true);
  await expect(page.locator('.infotip-pop')).toHaveText(OWNER_TEXT);
  await expect(tip).toHaveAttribute('aria-expanded', 'true');

  await tip.click();
  await expect.poll(() => popoverShown(page)).toBe(false);
  await expect(tip).toHaveAttribute('aria-expanded', 'false');
});

test('Enter and Space open it from the keyboard', async ({ page }) => {
  const { tip } = await openOwnerTip(page);
  await tip.focus();
  await page.keyboard.press('Enter');
  await expect.poll(() => popoverShown(page)).toBe(true);
  await page.keyboard.press('Enter');
  await expect.poll(() => popoverShown(page)).toBe(false);

  await tip.focus();
  await page.keyboard.press('Space');
  await expect.poll(() => popoverShown(page)).toBe(true);
});

test('Escape closes the explanation and leaves the dialog open', async ({ page }) => {
  const { dialog, tip } = await openOwnerTip(page);
  await tip.click();
  await expect.poll(() => popoverShown(page)).toBe(true);

  await page.keyboard.press('Escape');
  await expect.poll(() => popoverShown(page)).toBe(false);
  await expect(dialog).toBeVisible();
  // Focus goes back to the ⓘ, so a second Escape is the one that closes the dialog.
  await expect(tip).toBeFocused();

  // Both directions: with nothing open above it, Escape still closes the dialog.
  await page.keyboard.press('Escape');
  await expect(dialog).toBeHidden();
});

test('Tab stays inside the dialog while the explanation is open', async ({ page }) => {
  const { tip } = await openOwnerTip(page);
  await tip.click();
  await expect.poll(() => popoverShown(page)).toBe(true);

  // Enough presses to pass both ends of the dialog's focus order, in both directions.
  const outside: string[] = [];
  for (const key of [...Array(25).fill('Tab'), ...Array(25).fill('Shift+Tab')]) {
    await page.keyboard.press(key);
    const where = await page.evaluate(() => {
      const a = document.activeElement;
      if (!a || a.closest('[aria-modal="true"]')) return null;
      return `${a.tagName.toLowerCase()}.${(a as HTMLElement).className}`;
    });
    if (where) outside.push(`${key}: ${where}`);
  }
  expect(outside).toEqual([]);
  // The popover stayed open throughout, so the trap was tested with it in the document.
  expect(await popoverShown(page)).toBe(true);
});

test.describe('on a phone-width screen', () => {
  test.use({ viewport: { width: 390, height: 844 } });

  test('the explanation stays inside the screen', async ({ page }) => {
    const { tip } = await openOwnerTip(page);
    await tip.click();
    await expect.poll(() => popoverShown(page)).toBe(true);
    const box = await page.locator('.infotip-pop').boundingBox();
    if (!box) throw new Error('the explanation has no box');
    expect(box.x).toBeGreaterThanOrEqual(0);
    expect(box.x + box.width).toBeLessThanOrEqual(390);
    expect(box.y).toBeGreaterThanOrEqual(0);
    expect(box.y + box.height).toBeLessThanOrEqual(844);
  });
});
