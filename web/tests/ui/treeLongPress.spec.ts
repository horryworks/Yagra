// SPDX-License-Identifier: AGPL-3.0-only
// A finger's long press is the inventory tree's right-click (ADR-271).
//
// iOS Safari fires no `contextmenu` on a long press, and the rows were `draggable`, so a long press
// on a phone lifted a drag preview and nothing else: the menu's pin, maintenance, Poll now, pool,
// map and discovery items were out of reach. These drive real touch input through the DevTools
// protocol — Playwright's own `touchscreen` only taps — on the phone layout.
//
// ⚠️ This is Chromium pretending to be a phone. It cannot see what iOS Safari does with a long press
// (text selection, the magnifier, its own drag lift); that is checked on a real phone, by a person.

import type { CDPSession, Locator, Page } from '@playwright/test';
import { expect, test } from '../support/app';

test.use({ viewport: { width: 390, height: 844 }, hasTouch: true, isMobile: true });

/** The phone layout: the fixture pins `uiMode: 'desktop'`, under which width changes nothing. */
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode: 'auto' }, version: 0 }),
    );
  });
});

/** The selection lives in `?sel=`. */
const selected = (page: Page) => new URL(page.url()).searchParams.get('sel');

const rect = (loc: Locator) =>
  loc.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { top: r.top, bottom: r.bottom, left: r.left, height: r.height };
  });

/** One finger, driven through the protocol so the page sees `pointerType: 'touch'`. */
class Finger {
  private constructor(private readonly cdp: CDPSession) {}
  static async on(page: Page): Promise<Finger> {
    return new Finger(await page.context().newCDPSession(page));
  }
  down(x: number, y: number) {
    return this.cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [{ x, y }] });
  }
  move(x: number, y: number) {
    return this.cdp.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ x, y }] });
  }
  up() {
    return this.cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  }
}

async function openTree(page: Page): Promise<Locator> {
  await page.goto('/nodes');
  await expect(page.locator('html')).toHaveAttribute('data-viewport', 'mobile');
  // The emulation has to be a device with no hover, or this file proves nothing about one.
  expect(await page.evaluate(() => matchMedia('(hover: none)').matches)).toBe(true);
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  return rows;
}

async function centre(loc: Locator) {
  const r = await loc.boundingBox();
  if (!r) throw new Error('row has no box');
  return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
}

/** Press `row` for longer than a long press, check the menu opened while the finger was still
 *  down, lift, and give a lift's click time to land. */
async function longPress(page: Page, row: Locator) {
  const finger = await Finger.on(page);
  const at = await centre(row);
  await finger.down(at.x, at.y);
  await page.waitForTimeout(700);
  // Open while the finger is still down — the press itself is the feedback.
  await expect(page.getByRole('menu')).toBeVisible();
  await finger.up();
  await page.waitForTimeout(150);
}

test('a long press opens the menu below the row, and the lift neither closes it nor selects', async ({
  page,
}) => {
  await openTree(page);
  const folder = page.locator('.ntree-grow').first();
  const before = selected(page);
  await longPress(page, folder);

  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  expect(selected(page), 'the lift selected the row').toBe(before);
  // A folder's menu fits below its row here, so it is there: not under the finger.
  const row = await rect(folder);
  const box = await rect(menu);
  expect(box.top, 'the menu is under the finger, not below the row').toBeGreaterThanOrEqual(
    row.bottom - 1,
  );

  // Every item is a finger's height.
  const heights = await menu
    .locator(':scope > button')
    .evaluateAll((els) => els.map((el) => el.getBoundingClientRect().height));
  expect(heights.length).toBeGreaterThan(0);
  for (const h of heights) expect(h).toBeGreaterThanOrEqual(44);
});

// 🚨 A node's menu is taller than the room below its row, so it is moved up over the row — under the
// finger. Chromium sends the lift on as a mousedown and a click at that point: before the lift was
// cancelled, the mousedown closed the menu and the click pressed whichever item was there.
test('a menu that opens over the finger survives the lift, and no item is pressed', async ({
  page,
}) => {
  const rows = await openTree(page);
  const before = selected(page);
  await longPress(page, rows.nth(1));

  await expect(page.getByRole('menu')).toBeVisible();
  expect(selected(page)).toBe(before);
  await expect(page.getByRole('dialog')).toHaveCount(0);
});

test('a tap is still a tap: no menu, and the row is selected', async ({ page }) => {
  const rows = await openTree(page);
  const finger = await Finger.on(page);
  const at = await centre(rows.nth(0));

  await finger.down(at.x, at.y);
  await page.waitForTimeout(80);
  await finger.up();
  await page.waitForTimeout(700);

  await expect(page.getByRole('menu')).toHaveCount(0);
  await expect.poll(() => selected(page)).not.toBeNull();
});

test('a finger that moves is a scroll, not a press', async ({ page }) => {
  const rows = await openTree(page);
  const finger = await Finger.on(page);
  const at = await centre(rows.nth(0));

  await finger.down(at.x, at.y);
  await finger.move(at.x, at.y + 30);
  await page.waitForTimeout(700);
  await finger.up();

  await expect(page.getByRole('menu')).toHaveCount(0);
});

test('the rows are not draggable on a device with no hover', async ({ page }) => {
  const rows = await openTree(page);
  // The mock signs in as Admin, so a mouse would get draggable rows here.
  await expect(rows.first()).toHaveAttribute('draggable', 'false');
  await expect(page.locator('.ntree-grow').first()).toHaveAttribute('draggable', 'false');
});
