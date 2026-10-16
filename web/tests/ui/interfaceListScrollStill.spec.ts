// SPDX-License-Identifier: AGPL-3.0-only
// The Interfaces list stays where the operator scrolled it (ADR-203 item 7).
//
// The list keeps the selected row clear of the dock — on selection, and again whenever the list
// resizes, because the dock grows once its charts load. The second half ran on EVERY resize, so a
// row the operator had scrolled away from was pulled back the next time anything changed the
// list's height: a pane resize, the dock's charts finishing, an error line appearing.
//
// ⚠️ The generated mock has one interface, and a one-row list cannot scroll — sixty here, built
// from the generated row, and the precondition asserted.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ab';
const PORTS = 60;

function interfaces(): Json {
  const [template] = defaultBodyFor('/api/v1/nodes/{node_id}/interfaces') as Record<string, Json>[];
  return Array.from({ length: PORTS }, (_, i) => ({
    ...template,
    ifindex: i + 1,
    name: `port-${String(i + 1).padStart(2, '0')}`,
  })) as unknown as Json;
}

test.use({
  viewport: { width: 1280, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces': () => interfaces(),
    },
  },
});

test('a resize does not pull the list back to the selected row once the operator scrolled away', async ({
  page,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  const names = page.locator('.nd-if-row .nd-if-name');
  await expect(names.first()).toBeVisible({ timeout: 15_000 });
  await names.first().click();
  await expect(page.locator('.nd-if-dock')).toBeVisible();
  await page.waitForTimeout(300);

  const list = page.locator('.nd-if-list').first();
  const room = await list.evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(room, 'the list fits — this fixture cannot see the bug').toBeGreaterThan(400);

  const box = (await list.boundingBox())!;
  await page.mouse.move(box.x + 200, box.y + 80);
  for (let i = 0; i < 6; i++) await page.mouse.wheel(0, 400);
  await expect.poll(() => list.evaluate((el) => el.scrollTop)).toBeGreaterThan(300);
  await page.waitForTimeout(200);
  const parked = await list.evaluate((el) => el.scrollTop);

  // Anything that changes the list's height. A window resize is the one a test can make honestly.
  await page.setViewportSize({ width: 1280, height: 940 });
  await page.waitForTimeout(400);
  expect(
    await list.evaluate((el) => el.scrollTop),
    'the resize scrolled the list back to the selected row',
  ).toBeGreaterThan(parked - 80);
});

test('the selected row is still kept clear of the dock while it is in view', async ({ page }) => {
  // THE ACCEPT SIDE: the follow exists so the selected row is not hidden under the dock. A fix that
  // stopped following altogether would satisfy the test above.
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  const names = page.locator('.nd-if-row .nd-if-name');
  await expect(names.first()).toBeVisible({ timeout: 15_000 });
  const list = page.locator('.nd-if-list').first();
  // A row low in the list, so the dock opening over it is what would hide it.
  const lowY = await list.evaluate((el) => el.getBoundingClientRect().bottom - 20);
  const lowRow = page.locator('.nd-if-row').filter({ has: page.locator('.nd-if-name') });
  const candidates = await lowRow.evaluateAll(
    (rows, y) => rows.map((r, i) => ({ i, top: r.getBoundingClientRect().top })).filter((r) => r.top < y),
    lowY,
  );
  const target = candidates.at(-1)!.i;
  await lowRow.nth(target).locator('.nd-if-name').click();
  await expect(page.locator('.nd-if-dock')).toBeVisible();
  await page.waitForTimeout(400);
  const listBox = (await list.boundingBox())!;
  const rowBox = (await page.locator('.nd-if-row.selected').boundingBox())!;
  expect(rowBox.y + rowBox.height, 'the selected row is hidden under the dock').toBeLessThanOrEqual(
    listBox.y + listBox.height + 1,
  );
});
