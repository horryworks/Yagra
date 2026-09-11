// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's right-click menu, low on a short screen (ADR-124 Inc.2).
//
// `.ntree-menu` was a `position: fixed` box at the raw pointer coordinates — no clamp, no flip, no
// portal. Right-click low on the screen and the menu ran off the bottom, and the items cut off were
// the last ones: "Move N selected…". The operator saw "Open" and one "Move to group…", pressed it,
// and one node moved. That is the multi-select report this increment was opened for; the clipping
// caused it, so the clipping is pinned here on its own, and the move items in
// `treeMultiSelect.spec.ts`.
//
// 🚨 Every assertion here reads geometry or a computed style. `toBeVisible()` is true for an element
// laid out below the fold, so it cannot see the defect (ADR-088).

import { expect, test } from '../support/app';

/** The selection lives in `?sel=`. */
const selected = (page: { url(): string }) => new URL(page.url()).searchParams.get('sel');

/** Two animation frames — long enough for a document-level handler to have undone a click. */
async function settle(page: import('@playwright/test').Page) {
  await page.evaluate(
    () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r(null)))),
  );
}

/** Where an element actually is, in viewport coordinates. */
const rect = (loc: import('@playwright/test').Locator) =>
  loc.evaluate((el) => {
    const r = el.getBoundingClientRect();
    return { top: r.top, bottom: r.bottom, left: r.left, right: r.right };
  });

/** The painted background, as the operator's eye gets it. */
const bg = (loc: import('@playwright/test').Locator) =>
  loc.evaluate((el) => getComputedStyle(el).backgroundColor);

test.describe('on a short screen', () => {
  // Short on purpose. The node menu carries pool chips and two suppression sections; at this
  // height it fits neither below the row, nor above it, nor in the viewport whole — every branch
  // of the placement has to give way in turn, and the last one is "pin it to the top and scroll".
  // Only this test runs here: at 360px the tree body has no blank space left below its rows, so
  // the blank-click test below needs the default viewport.
  test.use({ viewport: { width: 1280, height: 360 } });

  test('a right-click low on it opens a menu that stays inside the viewport', async ({ page }) => {
    await page.goto('/nodes');
    const rows = page.locator('.ntree-node');
    await expect(rows).toHaveCount(3);
    await rows.nth(2).click({ button: 'right' });

    const menu = page.getByRole('menu');
    await expect(menu).toBeVisible();
    const vp = page.viewportSize();
    expect(vp).not.toBeNull();
    if (!vp) return;

    const box = await rect(menu);
    expect(box.top, 'menu is off the top edge').toBeGreaterThanOrEqual(0);
    expect(box.left, 'menu is off the left edge').toBeGreaterThanOrEqual(0);
    expect(box.right, 'menu overflows the right edge').toBeLessThanOrEqual(vp.width);
    expect(box.bottom, 'menu runs off the bottom of the screen').toBeLessThanOrEqual(vp.height);

    // The precondition that makes the assertion above a test of the scrolling branch rather than
    // of a menu that happened to fit: the content is taller than the box it was given.
    const clipped = await menu.evaluate((el) => el.scrollHeight > el.clientHeight);
    expect(clipped, 'the menu fits at this height — lower the viewport until it does not').toBe(
      true,
    );

    // The last item is the one that used to be cut off. Reaching it means scrolling the menu
    // itself, and once reached it must sit inside the viewport too.
    const last = menu.locator('button').last();
    await last.scrollIntoViewIfNeeded();
    const lastBox = await rect(last);
    expect(lastBox.top).toBeGreaterThanOrEqual(0);
    expect(lastBox.bottom, 'the last item is still below the fold').toBeLessThanOrEqual(vp.height);
  });
});

test("a folder's menu offers both sort directions, on screen", async ({ page }) => {
  // ADR-130. Two items rather than a toggle, so both have to be reachable — and "reachable" is a
  // geometry question here for the same reason the clipping test above exists: this menu grew two
  // more items, and the ones that fall off the bottom are the ones nobody presses.
  await page.goto('/nodes');
  const folders = page.locator('.ntree-grow');
  await expect(folders.first()).toBeVisible();
  await folders.first().click({ button: 'right' });

  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();
  const vp = page.viewportSize();
  expect(vp).not.toBeNull();
  if (!vp) return;

  for (const name of ['Sort ascending', 'Sort descending']) {
    const item = menu.getByRole('button', { name, exact: true });
    await expect(item, name + ' is not in the folder menu').toHaveCount(1);
    // 🚨 Not `toBeVisible()` — that is true for an item laid out below the fold (ADR-088).
    const box = await rect(item);
    expect(box.top, name + ' is off the top edge').toBeGreaterThanOrEqual(0);
    expect(box.bottom, name + ' is below the fold').toBeLessThanOrEqual(vp.height);
    expect(box.bottom - box.top, name + ' has no height').toBeGreaterThan(0);
  }
});

test('a hovered item is painted in a colour the menu itself is not', async ({ page }) => {
  // The surface is `.apop` (`--bg-secondary`) since the migration, and every hover in the old
  // stylesheet was `--bg-secondary` too — a hover painted in the surface colour is no hover. The
  // same hole as Inc.1's checked row: the class is there, the rule is there, and nothing but a
  // browser reading the colour can tell.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click({ button: 'right' });
  const menu = page.getByRole('menu');
  await expect(menu).toBeVisible();

  const surface = await bg(menu);
  const item = menu.getByRole('button', { name: 'Open', exact: true });
  const resting = await bg(item);
  await item.hover();
  const hovered = await bg(item);
  expect(hovered, 'hovering an item paints nothing').not.toBe(resting);
  expect(hovered, 'the hover colour is the surface colour, so it marks nothing').not.toBe(surface);
});

test('a click on blank space closes the menu and leaves the selection alone', async ({ page }) => {
  // ADR-073 決定 4, transient first. `AnchoredPopover` dismisses on mousedown, so by the time the
  // click reaches the tree body the menu is already gone — a body that read the menu's state at
  // click time would clear `?sel=` in the same press. It reads what it saw at mousedown instead.
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  const before = selected(page);
  expect(before).toMatch(/^node:/);

  await rows.nth(1).click({ button: 'right', position: { x: 200, y: 10 } });
  await expect(page.getByRole('menu')).toBeVisible();

  // Aim at the bottom-left of the scroller, and assert first that the point is actually blank —
  // not a row, and not the menu.
  const scroller = page.locator('.ntree-body');
  const box = await scroller.boundingBox();
  expect(box, 'the tree body has no box').not.toBeNull();
  const point = { x: 12, y: box!.height - 8 };
  const onBlank = await scroller.evaluate((el, p) => {
    const r = el.getBoundingClientRect();
    return document.elementFromPoint(r.left + p.x, r.top + p.y) === el;
  }, point);
  expect(onBlank, 'the bottom of the tree is covered — pick a different point').toBe(true);

  await scroller.click({ position: point });
  await expect(page.getByRole('menu')).toHaveCount(0);
  await settle(page);
  expect(selected(page), 'closing the menu also cleared the selection').toBe(before);
});
