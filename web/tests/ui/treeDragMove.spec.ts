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
import { TREE_SIBLING_IDS } from '../support/bootstrap';

/** What a `POST /api/v1/nodes/move` carried. `before`/`after` arrived with ADR-124 増分 8, which
 *  is what lets a drop between two rows carry more than one node. */
interface MoveRequest {
  node_ids: string[];
  group_id: string | null;
  before?: string;
  after?: string;
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

/** Where in the target row the pointer is when it lets go. A node row splits in half, so `top` is
 *  "before this row" and `middle`/`bottom` are "after it"; a group row's middle is "into it". */
type DropBand = 'top' | 'middle' | 'bottom';

/** Drop on the nth `.ntree-node` row, at the given height within it.
 *
 *  ⚠️ Indexed the way `startDrag` is, never by a CSS positional selector: the tree is virtualized,
 *  so a row is not the nth child of anything and `:nth-of-type(3)` matches nothing at all. */
async function dropOnNode(page: Page, index: number, band: DropBand = 'middle') {
  await page.evaluate(
    ({ index, band }: { index: number; band: DropBand }) => {
      const w = window as unknown as { __dt?: DataTransfer };
      const dst = document.querySelectorAll('.ntree-node')[index];
      if (!dst) throw new Error(`no node row at index ${index}`);
      const r = dst.getBoundingClientRect();
      const frac = band === 'top' ? 0.2 : band === 'bottom' ? 0.8 : 0.5;
      const init: DragEventInit = {
        bubbles: true,
        cancelable: true,
        dataTransfer: w.__dt,
        clientX: r.left + r.width / 2,
        clientY: r.top + r.height * frac,
      };
      dst.dispatchEvent(new DragEvent('dragover', init));
      dst.dispatchEvent(new DragEvent('drop', init));
    },
    { index, band },
  );
}

/** Begin a drag on the row matching `selector` — the folder rows, which have no index. */
async function startDragOn(page: Page, selector: string) {
  await page.evaluate((sel: string) => {
    const w = window as unknown as { __dt?: DataTransfer };
    w.__dt = new DataTransfer();
    const src = document.querySelector(sel);
    if (!src) throw new Error(`no drag source for ${sel}`);
    src.dispatchEvent(
      new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer: w.__dt }),
    );
  }, selector);
}

/** Hover the nth `.ntree-node` row without letting go, so the drop indicator can be read. The
 *  `drop-bad` mark is the only thing a refused drag puts on screen, and the whole of ADR-162
 *  decision 6 is that the refusal is visible rather than a drop that quietly lands elsewhere. */
async function dragOverNode(page: Page, index: number, band: DropBand = 'middle') {
  await page.evaluate(
    ({ index, band }: { index: number; band: DropBand }) => {
      const w = window as unknown as { __dt?: DataTransfer };
      const dst = document.querySelectorAll('.ntree-node')[index];
      if (!dst) throw new Error(`no node row at index ${index}`);
      const r = dst.getBoundingClientRect();
      const frac = band === 'top' ? 0.2 : band === 'bottom' ? 0.8 : 0.5;
      dst.dispatchEvent(
        new DragEvent('dragover', {
          bubbles: true,
          cancelable: true,
          dataTransfer: w.__dt,
          clientX: r.left + r.width / 2,
          clientY: r.top + r.height * frac,
        }),
      );
    },
    { index, band },
  );
}

/** Drop on the row matching `selector`, at the given height within it. */
async function dropOn(page: Page, selector: string, band: DropBand = 'middle') {
  await page.evaluate(
    ({ sel, band }: { sel: string; band: DropBand }) => {
      const w = window as unknown as { __dt?: DataTransfer };
      const dst = document.querySelector(sel);
      if (!dst) throw new Error(`no drop target for ${sel}`);
      const r = dst.getBoundingClientRect();
      // Inset from the very edge: a drop exactly on a boundary lands on whichever row the browser
      // hit-tests, which is not the thing under test.
      const frac = band === 'top' ? 0.2 : band === 'bottom' ? 0.8 : 0.5;
      const init: DragEventInit = {
        bubbles: true,
        cancelable: true,
        dataTransfer: w.__dt,
        clientX: r.left + r.width / 2,
        clientY: r.top + r.height * frac,
      };
      dst.dispatchEvent(new DragEvent('dragover', init));
      dst.dispatchEvent(new DragEvent('drop', init));
    },
    { sel: selector, band },
  );
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

test('a batch dropped between two rows names the sibling it landed before', async ({ page }) => {
  // 🚨 ADR-124 増分 8, and the gesture that had no answer before it: Ctrl-select two rows, drop
  // them on the upper half of a third, and the whole batch lands *there* rather than at the end of
  // the folder. `dropPosition` used to force `inside` for any batch — an append — because
  // `PUT /nodes/{id}/placement` took one node and there was no bulk form to carry an insertion
  // point. So the same drop answered differently at one node and at three.
  //
  // ⚠️ The mock cannot show where the rows ended up (`/nodes/by-group` answers the same three
  // whatever it is asked), so what is assertable is the request: two ids AND the anchor. Asserting
  // only the ids would pass on the appending build this test exists to refuse.
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await rows.nth(0).click();
  await rows.nth(1).click({ modifiers: ['ControlOrMeta'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(2);

  await startDrag(page, 0);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(2);

  // The third row is outside the batch, so it is a legal destination.
  await dropOnNode(page, 2, 'top');
  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0].node_ids).toHaveLength(2);
  expect(moves[0].before, 'the batch was appended instead of placed where it was dropped').toBe(
    TREE_SIBLING_IDS[2],
  );
  expect(moves[0].after).toBeUndefined();
  // Spent, the same as every other batch move on this screen.
  await expect(page.locator('.ntree-row.checked')).toHaveCount(0);
});

test('dropping on the lower half of a row names it as the row to follow', async ({ page }) => {
  // The other band, and the one that proves the drop reads the cursor rather than always saying
  // `before`. One node, so this also pins that a single drag takes the same request shape.
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);

  await startDrag(page, 0);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(1);
  await dropOnNode(page, 2, 'bottom');

  await expect.poll(() => moves.length).toBe(1);
  expect(moves[0].node_ids).toHaveLength(1);
  expect(moves[0].after).toBe(TREE_SIBLING_IDS[2]);
  expect(moves[0].before).toBeUndefined();
});

test('a folder dragged onto an ungrouped node is refused, and the same drag works on the header', async ({
  page,
}) => {
  // ADR-162 decision 6, in the browser. A folder may now be dropped beside a node — but NOT beside
  // a node in the "Ungrouped" bucket, which is drawn apart from the top-level folders. The server
  // would compute a real position there (both live in the same sibling scope), and the folder would
  // then appear somewhere the operator did not drop it.
  //
  // ⚠️ **The mock's three nodes are all ungrouped** (`tests/support/bootstrap.ts`), so this tier can
  // only reach the refusing half. The permitting half — a folder beside a node *inside* a folder —
  // is `nodeTreeDnd.test.ts`'s `lets a folder land beside a node inside another folder`, plus a
  // `/flashdeploy` eyeball. Changing the fixture to file one node in the folder would move four
  // other specs that count `.ntree-node`.
  //
  // 🚨 **The second half is what makes the first half mean anything.** "No request was sent" is
  // also what a broken drag looks like, so the same drag is then dropped on the Ungrouped header
  // and must produce one — a positive control in the same test, on the same payload.
  const placements: string[] = [];
  await page.route(
    '**/api/v1/node-groups/*/placement',
    async (route: import('@playwright/test').Route) => {
      placements.push(route.request().url());
      await route.fulfill({ status: 204, body: '' });
    },
  );
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  await expect(page.locator('.ntree-node')).toHaveCount(3);
  const folder = '.ntree-row.ntree-grow';
  await expect(page.locator(folder).first()).toBeVisible();

  await startDragOn(page, folder);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(1);

  // Hover the last ungrouped node: the row must say no, in the one way a drag can.
  await dragOverNode(page, 2, 'top');
  await expect(page.locator('.ntree-row.drop-bad')).toHaveCount(1);
  await dropOnNode(page, 2, 'top');
  await page.waitForTimeout(300);
  expect(placements, 'a folder was placed among the ungrouped nodes').toHaveLength(0);
  expect(moves, 'a folder drop must never send a node move').toHaveLength(0);

  // The positive control, same gesture: the Ungrouped header is the root drop zone and re-parents
  // the folder to the top level. The drag has to be started again — a drop ends it, which is the
  // behaviour every other test on this page relies on.
  await startDragOn(page, folder);
  await expect(page.locator('.ntree-row.dragging')).toHaveCount(1);
  await dropOn(page, '.ntree-ungrouped-head');
  await expect.poll(() => placements.length).toBe(1);
  await expect.poll(() => placements.length).toBe(1);
});
