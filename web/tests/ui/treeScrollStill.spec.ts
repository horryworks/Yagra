// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree does not move unless the operator scrolls it (ADR-124 増分 5).
//
// TWO CAUSES SHIPPED, and neither was a scroll call — `scrollTo` / `scrollIntoView` / `scrollTop`
// are written nowhere in `NodeTree.tsx` or `NodesPage.tsx`. The browser did both, which is exactly
// why nothing but a browser can see them:
//
//   1. **The working-set bar stole the scroller's height.** It rendered above `.ntree-body`
//      (`flex: 1`), so the first Ctrl click dropped the scroller's top edge ~60px and translated
//      every visible row down two rows.
//      🚨 **`scrollTop` never changed for this one.** A `scrollTop` assertion — the obvious thing
//      to write — passes against the shipped bug. What moved was the scroller's *box*, so the
//      assertion has to be geometric: a named row's `y`, and the scroller's own `y`.
//   2. **Click-focus scrolling.** `.ntree-node-name` is a real `<button>` at `flex: 1`, covering
//      nearly the whole row, and the browser scrolls a newly-focused element into view when it is
//      not fully visible. On a 30px grid the end rows are almost always half-clipped ⇒ up to 29px
//      per click. This one *is* `scrollTop`.
//
// ➕ **A third was suspected and measured away**: Shift-click extending a document selection across
// the rows. It does not happen — the row carries `draggable`, and Chromium's UA stylesheet already
// computes `user-select: none` for a draggable element. See the Shift test for the measurement and
// for why there is no `getSelection()` assertion in this file.
//
// ⚠️ **The default mock is three nodes, and a three-row tree cannot scroll.** ADR-073's rule
// ("count the gestures against the real data, not against the layout") applies to a fixture too:
// against a tree that fits its pane, every assertion here reports "nothing moved" for the same
// reason a dead selector would. Hence the 60 below, and the precondition asserted in `parkAt`.
//
// 🚨 **No row-count assertion, and no `.nth(i)`.** With 60 rows only the virtualized window is in
// the DOM, so a count measures the window and an index re-resolves to a different element after
// every scroll-driven re-render. Rows are addressed by name, the way `rowMenu.spec.ts` does.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';
import type { components } from '../../src/api/schema';

type Page = import('@playwright/test').Page;

const NODE_COUNT = 60;

/** The generated member row, repeated into an ungrouped bucket long enough to virtualize. Built
 *  from the generated shape rather than hand-written, so a change to `NodeSummary` reaches here.
 *  ⚠️ `group_id: null` is what files them under Ungrouped — the generator fills every nullable
 *  uuid, and a node claiming a folder nobody opened is a node with no row. */
const UNGROUPED = (() => {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as components['schemas']['GroupNodes'];
  const [template] = body.nodes;
  return {
    ...body,
    nodes: Array.from({ length: NODE_COUNT }, (_, i) => ({
      ...template,
      id: `00000000-0000-4000-8000-a${String(i).padStart(11, '0')}`,
      name: `${MOCK_PREFIX}node-${String(i).padStart(2, '0')}`,
      group_id: null,
      sort_order: i + 1,
    })),
  };
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      // ⚠️ One call per open folder plus one for the bucket, all landing on this key — so the
      // answer has to depend on the query. Returning the 60 to a folder as well would put every
      // id in the flat row list twice, which is a broken tree, not a taller one.
      '/api/v1/nodes/by-group': (url: URL) =>
        (url.searchParams.get('group') ? { ...UNGROUPED, nodes: [] } : UNGROUPED) as unknown as Json,
    },
  },
});

/** A node row by name — stable across the re-renders scrolling causes. */
const row = (page: Page, i: number) =>
  page.locator('.ntree-node').filter({ hasText: `${MOCK_PREFIX}node-${String(i).padStart(2, '0')}` });

const scrollTopOf = (page: Page) => page.locator('.ntree-body').evaluate((el) => el.scrollTop);

/**
 * Park the scroller at `top` and let the virtualizer render that window.
 *
 * Asserts the precondition rather than assuming it: on a tree that fits its pane every assertion
 * in this file passes without testing anything (`floor-must-count-what-was-checked`).
 */
async function parkAt(page: Page, top: number) {
  const scroller = page.locator('.ntree-body');
  await expect(row(page, 0)).toBeVisible();
  const room = await scroller.evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(room, 'the tree does not scroll — this fixture cannot see the bug').toBeGreaterThan(300);
  await scroller.evaluate((el, y) => {
    el.scrollTop = y;
  }, top);
  await page.waitForTimeout(60);
  return scroller;
}

/** The topmost node row currently rendered, by its index in the fixture. */
async function firstRenderedIndex(page: Page): Promise<number> {
  const names = await page.locator('.ntree-node .ntree-node-name').allInnerTexts();
  const first = names.find((n) => n.startsWith(`${MOCK_PREFIX}node-`));
  expect(first, 'no node row is rendered').toBeTruthy();
  return Number(first!.slice(`${MOCK_PREFIX}node-`.length));
}

/** The index of a row that is fully on screen inside the scroller, `nth` from the top edge. */
async function visibleIndex(page: Page, nth: number): Promise<number> {
  return page.locator('.ntree-body').evaluate((el, n) => {
    const box = el.getBoundingClientRect();
    const rows = [...el.querySelectorAll<HTMLElement>('.ntree-node')];
    const on = rows.filter((r) => {
      const b = r.getBoundingClientRect();
      return b.top >= box.top && b.bottom <= box.bottom;
    });
    const name = on[n]?.querySelector<HTMLElement>('.ntree-node-name')?.innerText ?? '';
    return Number(name.slice(name.lastIndexOf('-') + 1));
  }, nth);
}

test('Ctrl-clicking rows leaves every row exactly where it was', async ({ page }) => {
  // 🚨 THE REGRESSION for cause 1, in the gesture the report used. Assert GEOMETRY, not
  // `scrollTop` — the bar rendering above the tree moved the scroller's box while `scrollTop`
  // stayed put, so the obvious assertion passes against the shipped bug.
  await page.goto('/nodes');
  await parkAt(page, 300);

  const i = await visibleIndex(page, 2);
  const anchor = row(page, i);
  await expect(anchor).toBeVisible();
  const before = await anchor.boundingBox();
  const paneBefore = await page.locator('.ntree-body').boundingBox();

  await row(page, await visibleIndex(page, 1)).click({ modifiers: ['ControlOrMeta'] });
  await row(page, await visibleIndex(page, 3)).click({ modifiers: ['ControlOrMeta'] });

  // The bar is on screen, so the geometry below is measured with it rendered — not before it.
  await expect(page.locator('.nodes-selbar')).toBeVisible();
  expect(await page.locator('.ntree-row.checked').count()).toBeGreaterThanOrEqual(2);

  const after = await anchor.boundingBox();
  const paneAfter = await page.locator('.ntree-body').boundingBox();
  expect(after!.y, 'the row the operator was looking at moved').toBeCloseTo(before!.y, 0);
  expect(paneAfter!.y, 'the scroller top edge moved').toBeCloseTo(paneBefore!.y, 0);
});

test('the scroller does not move when a half-clipped row is clicked', async ({ page }) => {
  // 🚨 THE REGRESSION for cause 2. 137 is deliberately not a multiple of `--row-h` (30px), so the
  // rows at both ends of the pane are cut — which is the state in which the browser scrolls a
  // freshly focused button into view. Pre-fix the pane moves by up to 29px.
  //
  // 🚨 **`locator.click()` cannot be used here, and that is not a detail.** Playwright's
  // actionability checks run `scrollIntoViewIfNeeded` on the target first, so clicking a clipped
  // row scrolls the pane *before* the app sees a single event — measured at 2px, which reads
  // exactly like the defect. `page.mouse.click` at a point dispatches the events and scrolls
  // nothing, so what this test measures is the application.
  await page.goto('/nodes');
  await parkAt(page, 137);
  expect(await scrollTopOf(page)).toBe(137);

  // The bottom-most row that is only partly on screen: the one the browser wants to pull up.
  const point = await page.locator('.ntree-body').evaluate((el) => {
    const box = el.getBoundingClientRect();
    const cut = [...el.querySelectorAll<HTMLElement>('.ntree-node')].find((r) => {
      const b = r.getBoundingClientRect();
      return b.top < box.bottom && b.bottom > box.bottom;
    });
    const name = cut?.querySelector<HTMLElement>('.ntree-node-name');
    if (!name) return null;
    const nb = name.getBoundingClientRect();
    // Inside the button AND inside the visible strip of the clipped row.
    return { x: nb.left + 8, y: (Math.max(nb.top, box.top) + Math.min(nb.bottom, box.bottom)) / 2 };
  });
  expect(point, 'no row is clipped at the bottom edge — nothing here could scroll').not.toBeNull();

  await page.keyboard.down('Control');
  await page.mouse.click(point!.x, point!.y);
  await page.keyboard.up('Control');

  await expect(page.locator('.nodes-selbar')).toBeVisible();
  expect(await scrollTopOf(page), 'focusing a clipped row scrolled the tree').toBe(137);
});

test('a Shift range does not move the tree', async ({ page }) => {
  // The other modified gesture, over the same guard. Two clicks and a state write per click, so it
  // is the one most likely to move the pane if any of the three mechanisms is still live.
  //
  // 🚨 **There is deliberately no `window.getSelection()` assertion here, and the reason is worth
  // keeping.** The increment was opened believing a Shift click also painted a document selection
  // across the rows. Measured in Chromium: it does not. The row carries `draggable`, and the UA
  // stylesheet gives a draggable element `user-select: none` already — flipping `draggable` off in
  // the live page turns the computed value from `none` to `auto`. An assertion that the selection
  // is empty therefore passes with `NodeTree.css`'s own `user-select` rule **deleted**, which was
  // measured too. A test that cannot fail is worse than no test: it reads as coverage.
  await page.goto('/nodes');
  await parkAt(page, 240);

  const from = await visibleIndex(page, 1);
  const to = await visibleIndex(page, 6);
  await row(page, from).click();
  await row(page, to).click({ modifiers: ['Shift'] });
  expect(await page.locator('.ntree-row.checked').count()).toBeGreaterThan(2);

  expect(await scrollTopOf(page), 'a Shift range scrolled the tree').toBe(240);
});

test('the working-set bar sits below the tree', async ({ page }) => {
  // The structural pin behind 増分 5 決定 A. Without it a later refactor can put the bar back above
  // the tree and only the first test fails, with a message about a row's `y` rather than about
  // where the bar is.
  await page.goto('/nodes');
  await parkAt(page, 0);
  await row(page, await firstRenderedIndex(page)).click({ modifiers: ['ControlOrMeta'] });

  const bar = await page.locator('.nodes-selbar').boundingBox();
  const pane = await page.locator('.ntree-body').boundingBox();
  expect(bar, 'the bar did not render').toBeTruthy();
  expect(bar!.y, 'the bar is above the tree, which is what moved every row').toBeGreaterThanOrEqual(
    pane!.y + pane!.height - 1,
  );
});

test('a keyboard focus may still scroll a row into view', async ({ page }) => {
  // 🚨 THE ACCEPT SIDE, and the file is worthless without it: a guard that pinned the scroller
  // unconditionally satisfies every assertion above
  // (`rejection-only-tests-pass-when-everything-rejects`). A keyboard focus has no preceding
  // press, and the browser showing the operator what it just focused is then CORRECT — undoing it
  // would make the tree unreachable by keyboard.
  await page.goto('/nodes');
  await parkAt(page, 0);
  expect(await scrollTopOf(page)).toBe(0);

  // A row rendered by the overscan window but below the fold — in the DOM, out of sight, and with
  // no mousedown anywhere near it.
  const moved = await page.locator('.ntree-body').evaluate((el) => {
    const box = el.getBoundingClientRect();
    const names = [...el.querySelectorAll<HTMLElement>('.ntree-node .ntree-node-name')];
    const below = names.find((n) => n.getBoundingClientRect().top > box.bottom);
    if (!below) return null;
    below.focus();
    return el.scrollTop;
  });
  expect(moved, 'the overscan window rendered nothing below the fold to focus').not.toBeNull();
  expect(
    moved,
    'a keyboard focus was refused its scroll, so the tree is keyboard-unreachable',
  ).toBeGreaterThan(0);
});
