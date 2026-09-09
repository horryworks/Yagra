// SPDX-License-Identifier: AGPL-3.0-only
// Dragging nodes in the inventory tree (ADR-124 Inc.4).
//
// 🚨 **This is the first drag-and-drop test in the repository, and its absence is why the defect
// shipped.** `nodeTreeDnd.test.ts` had seventeen cases and every one of them handed the judgement a
// single-id payload, so the branch that mattered was covered and the *value* it branched on was
// never questioned. Nothing at any tier had ever performed the gesture.
//
// ⚠️ **The mock cannot show the move's effect.** `/api/v1/nodes/by-group` answers the same three
// ungrouped nodes whatever it is asked (see `tests/support/bootstrap.ts`), so the tree looks
// identical after a successful move. What is assertable here is the **request** — which ids left
// the browser — and that is exactly the thing that was wrong: the drop sent one id when three rows
// were marked. The rows landing in the folder is a `/flashdeploy` eyeball.
//
// ⚠️ **HTML5 drag events are dispatched by hand, in two steps, and the split is load-bearing.** The
// payload lives in React state, not in `dataTransfer`, so `dragover`/`drop` fired in the same tick
// as `dragstart` run against the render that has not seen `setDrag` yet — `drag` is still null and
// the drop returns having done nothing. Waiting for `.dragging` to appear between the two is both
// the synchronisation point and an assertion about the fix (every row that will move dims, not just
// the one the pointer grabbed).

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';

/** What a `POST /api/v1/nodes/move` carried. */
interface MoveRequest {
  node_ids: string[];
  group_id: string | null;
}

/** Answer the bulk move ourselves and record what was asked, so a partial result cannot appear
 *  from a generated body and put an error banner on the screen. */
async function captureMoves(page: Page): Promise<MoveRequest[]> {
  const seen: MoveRequest[] = [];
  await page.route('**/api/v1/nodes/move', async (route: import('@playwright/test').Route) => {
    const body = route.request().postDataJSON() as MoveRequest;
    seen.push(body);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, moved: body.node_ids.length }),
    });
  });
  return seen;
}

/** Begin a drag on the nth `.ntree-node` row. The `DataTransfer` is parked on `window` because the
 *  drop happens in a later evaluate and both events must carry the same one. */
async function startDrag(page: Page, index: number) {
  await page.evaluate((i: number) => {
    const w = window as unknown as { __dt?: DataTransfer };
    w.__dt = new DataTransfer();
    const src = document.querySelectorAll('.ntree-node')[i];
    src.dispatchEvent(
      new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer: w.__dt }),
    );
  }, index);
}

/** Drop on the row matching `selector`, over its vertical middle. */
async function dropOn(page: Page, selector: string) {
  await page.evaluate((sel: string) => {
    const w = window as unknown as { __dt?: DataTransfer };
    const dst = document.querySelector(sel);
    if (!dst) throw new Error(`no drop target for ${sel}`);
    const r = dst.getBoundingClientRect();
    const init: DragEventInit = {
      bubbles: true,
      cancelable: true,
      dataTransfer: w.__dt,
      clientX: r.left + r.width / 2,
      clientY: r.top + r.height / 2,
    };
    dst.dispatchEvent(new DragEvent('dragover', init));
    dst.dispatchEvent(new DragEvent('drop', init));
  }, selector);
}

test('dragging a checked row moves the whole working set', async ({ page }) => {
  // 🚨 THE REGRESSION, in the gesture the report used: Ctrl-select three rows, drag one of them
  // onto a folder, and watch one node move.
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  const folder = '.ntree-row.ntree-grow';
  await expect(page.locator(folder).first()).toBeVisible();

  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });
  await rows.nth(2).click({ modifiers: ['ControlOrMeta'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);

  await startDrag(page, 1);
  // Every row that will move dims. This said ONE until Inc.4 — the grabbed row — which was the
  // defect visible on screen for a release before anyone read it.
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(3);

  await dropOn(page, folder);
  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0].node_ids, 'the drop moved fewer nodes than were marked').toHaveLength(3);
  expect(moves[0].group_id).not.toBeNull();
  // And the batch is spent: leaving it would point at rows that have already been filed.
  await expect(page.locator('.ntree-row.checked')).toHaveCount(0);
});

test('dragging a row outside the working set moves only that row', async ({ page }) => {
  // The other half of the rule, and the one that stops the fix from over-reaching: the gesture
  // belongs to the row the pointer actually grabbed. Same answer the right-click menu gives.
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(2);

  await startDrag(page, 2);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(1);

  await dropOn(page, '.ntree-row.ntree-grow');
  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0].node_ids).toHaveLength(1);
  // Untouched — the batch was not what moved, so it is still there to act on.
  await expect(page.locator('.ntree-row.checked')).toHaveCount(2);
});

test('one node with nothing checked still goes through the same request', async ({ page }) => {
  // Inc.4 決定 D: one node is a list of one. Before it, this gesture took the single-node PUT and
  // the dialogs took the bulk POST, so "what happens when you move something" had two answers.
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await startDrag(page, 0);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(1);
  await dropOn(page, '.ntree-row.ntree-grow');

  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0].node_ids).toHaveLength(1);
});

test('a drop that moved fewer nodes than it asked for says so, and the message survives', async ({
  page,
}) => {
  // 🚨 The order inside the handler is the whole test. `reload()` opens with `setError(null)`, so
  // reporting the shortfall before the refresh wipes it in the same tick — the operator sees a
  // clean screen and believes every node moved. The two numbers this endpoint returns exist for
  // exactly this case, and returning them is worth nothing if the screen throws the answer away.
  await page.route('**/api/v1/nodes/move', async (route: import('@playwright/test').Route) => {
    const body = route.request().postDataJSON() as MoveRequest;
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, moved: 0 }),
    });
  });
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await startDrag(page, 0);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(1);
  await dropOn(page, '.ntree-row.ntree-grow');

  const banner = page.locator('.form-error');
  await expect(banner).toBeVisible();
  await expect(banner).toContainText('0');
  // And it is still there a moment later, once the refresh this drop kicked off has settled.
  await page.waitForTimeout(500);
  await expect(banner).toBeVisible();
});
