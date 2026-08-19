// SPDX-License-Identifier: AGPL-3.0-only
// The All-nodes split handle (ADR-074) — dragging it, typing at it, and the reset.
//
// Why Tier1 and not a unit test: the arithmetic is unit-tested in `pages/nodesPaneWidth.test.ts`,
// but nothing there can say whether the handle is where the CSS grid puts it, whether the pane
// actually took the width, or whether the drag survives pointer capture. Those are layout and
// wiring, which is what a browser is for.
//
// ⚠️ The assertions never name a pixel the CSS chose. `1fr` and the window size decide the detail
// pane, so the tests assert *relations* — it grew, the other one shrank, the total held.

import { expect, test } from '../support/app';

/** Rendered width of one pane, as the browser laid it out. */
async function paneWidth(page: import('@playwright/test').Page, selector: string) {
  return page.locator(selector).first().evaluate((el) => el.getBoundingClientRect().width);
}

const TREE = '.nodes-split > .nodes-pane:not(.nodes-detail-pane)';
const DETAIL = '.nodes-detail-pane';

test('the handle sits between the two panes and is announced as a control', async ({ page }) => {
  await page.goto('/nodes');
  const handle = page.locator('.nodes-split-handle');
  await expect(handle).toBeVisible();
  await expect(handle).toHaveAttribute('role', 'slider');
  await expect(handle).toHaveAttribute('aria-orientation', 'horizontal');

  // Between them, not merely present: the seam must be to the right of the tree and left of the
  // detail, or the grid put it in the wrong column and every drag below would still "pass".
  const [tree, seam, detail] = await Promise.all([
    page.locator(TREE).boundingBox(),
    handle.boundingBox(),
    page.locator(DETAIL).boundingBox(),
  ]);
  expect(tree && seam && detail).toBeTruthy();
  expect(seam!.x).toBeGreaterThanOrEqual(tree!.x + tree!.width - 1);
  expect(seam!.x + seam!.width).toBeLessThanOrEqual(detail!.x + 1);
});

test('dragging the handle right widens the inventory and narrows the detail', async ({ page }) => {
  await page.goto('/nodes');
  const handle = page.locator('.nodes-split-handle');
  await expect(handle).toBeVisible();

  const before = await paneWidth(page, TREE);
  const detailBefore = await paneWidth(page, DETAIL);
  const box = (await handle.boundingBox())!;

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  // Two moves rather than one: the drag is computed from the gesture origin, so a single jump
  // would pass even if the handler accumulated deltas per event.
  await page.mouse.move(box.x + box.width / 2 + 60, box.y + box.height / 2);
  await page.mouse.move(box.x + box.width / 2 + 120, box.y + box.height / 2);
  await page.mouse.up();

  const after = await paneWidth(page, TREE);
  const detailAfter = await paneWidth(page, DETAIL);
  expect(after, 'the inventory did not widen').toBeGreaterThan(before + 80);
  expect(detailAfter, 'the detail pane did not give up the space').toBeLessThan(detailBefore - 80);
  // The split is a partition, not two independent sizes: what one gained the other lost.
  expect(Math.abs(after - before + (detailAfter - detailBefore))).toBeLessThan(3);
});

test('the gesture is written to the preference, and a double-click gives the default back', async ({
  page,
}) => {
  // ⚠️ This deliberately does NOT reload to prove persistence. `tests/support/app.ts` seeds
  // `yagra_prefs` from an `addInitScript`, which runs on *every* navigation and overwrites the
  // whole object — so a reload here would always report the default and the test would be about
  // the harness. What is left, and is the half that can actually break, is that one gesture writes
  // one value: the store is `persist`ed, so a written value is a remembered value.
  await page.goto('/nodes');
  const handle = page.locator('.nodes-split-handle');
  await expect(handle).toBeVisible();

  const stored = () =>
    page.evaluate(
      () =>
        (JSON.parse(localStorage.getItem('yagra_prefs') ?? '{}') as {
          state?: { nodesPaneWidth?: number | null };
        }).state?.nodesPaneWidth ?? null,
    );

  const original = await paneWidth(page, TREE);
  expect(await stored(), 'a width was stored before anything was dragged').toBeNull();

  const box = (await handle.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2 + 100, box.y + box.height / 2);
  await page.mouse.up();

  const dragged = await paneWidth(page, TREE);
  expect(dragged).toBeGreaterThan(original + 60);
  expect(Math.abs((await stored())! - dragged), 'the drag stored a different width than it drew')
    .toBeLessThan(3);

  await handle.dblclick();
  expect(await stored(), 'double-click left a width behind instead of clearing it').toBeNull();
  expect(
    Math.abs((await paneWidth(page, TREE)) - original),
    'double-click did not restore the default width',
  ).toBeLessThan(3);
});

test('the handle is operable from the keyboard', async ({ page }) => {
  await page.goto('/nodes');
  const handle = page.locator('.nodes-split-handle');
  await expect(handle).toBeVisible();

  const before = await paneWidth(page, TREE);
  await handle.focus();
  for (let i = 0; i < 4; i++) await page.keyboard.press('ArrowRight');
  const wider = await paneWidth(page, TREE);
  expect(wider, 'ArrowRight did not widen the inventory').toBeGreaterThan(before);

  for (let i = 0; i < 4; i++) await page.keyboard.press('ArrowLeft');
  expect(Math.abs((await paneWidth(page, TREE)) - before)).toBeLessThan(3);
});

test('collapsing the inventory to the rail takes the handle away', async ({ page }) => {
  // There is nothing to proportion against a 40px rail, and leaving the handle would put a
  // draggable seam beside a pane whose width the drag cannot change.
  await page.goto('/nodes');
  await expect(page.locator('.nodes-split-handle')).toBeVisible();
  await page.locator('.nodes-pane-collapse').click();
  await expect(page.locator('.nodes-split')).toHaveClass(/inv-collapsed/);
  await expect(page.locator('.nodes-split-handle')).toHaveCount(0);
});
