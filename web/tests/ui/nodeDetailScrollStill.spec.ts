// SPDX-License-Identifier: AGPL-3.0-only
// A save does not move the node detail pane (ADR-203).
//
// Two ways shipped, and both remounted the pane rather than scrolling it: Edit node from the tree's
// menu put a nonce in `NodeDetail`'s `key`, and Edit node from the pane's own header bumped a nonce
// that blanked the node to "Loading…" — which unmounts every tab. Either way `.nd-body` lost its
// offset and the operator was back at the top of the tab after every save. No scroll call is
// involved, so only a browser measuring the offset after the save can see it.
//
// ⚠️ The viewport is short on purpose: at the default size the Overview may fit its pane, and against
// a pane that cannot scroll "the offset did not change" holds for the same reason a dead selector
// would (`floor-must-count-what-was-checked`). The precondition is asserted, not assumed.

import { expect, test } from '../support/app';
import { TREE_SIBLING_IDS } from '../support/bootstrap';

type Page = import('@playwright/test').Page;

test.use({ viewport: { width: 1280, height: 520 } });

const body = (page: Page) => page.locator('.nodes-detail-pane .nd-body');
const offset = (page: Page) => body(page).evaluate((el) => el.scrollTop);

/** Open the node, scroll its Overview down with the wheel, and return where it was left. */
async function scrolledDetail(page: Page): Promise<number> {
  await page.goto(`/nodes?sel=node:${TREE_SIBLING_IDS[0]}`);
  await expect(page.locator('.nodes-detail-pane .nd-head')).toBeVisible();
  const room = await body(page).evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(room, 'the Overview fits its pane — this fixture cannot see the bug').toBeGreaterThan(150);
  const box = (await body(page).boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 60);
  await page.mouse.wheel(0, 120);
  await expect.poll(() => offset(page)).toBeGreaterThan(0);
  await page.waitForTimeout(100);
  return offset(page);
}

/** Press the dialog's Save and wait for it to close and the pane's re-read to land. */
async function save(page: Page) {
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const reread = page.waitForResponse(
    (r) => r.request().method() === 'GET' && /\/api\/v1\/nodes\/[^/]+$/.test(new URL(r.url()).pathname),
  );
  await dialog.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(dialog).toHaveCount(0);
  await reread;
  await page.waitForTimeout(300);
}

test("saving Edit node from the pane's header keeps the pane where it was", async ({ page }) => {
  const before = await scrolledDetail(page);
  await page.locator('.nodes-detail-pane .nd-head').getByRole('button', { name: 'Edit node' }).click();
  await save(page);
  await expect(page.locator('.nodes-detail-pane .nd-head')).toBeVisible();
  expect(await offset(page), 'the save scrolled the pane back to the top').toBe(before);
});

test("saving Edit node from the tree's menu keeps the pane where it was", async ({ page }) => {
  const before = await scrolledDetail(page);
  await page.locator('.ntree-row.sel').click({ button: 'right' });
  await page.getByRole('menu').getByRole('button', { name: 'Edit node…', exact: true }).click();
  await save(page);
  await expect(page.locator('.nodes-detail-pane .nd-head')).toBeVisible();
  expect(await offset(page), 'the save remounted the pane and lost its offset').toBe(before);
});
