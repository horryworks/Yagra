// SPDX-License-Identifier: AGPL-3.0-only
// The tree stays where it was across a reload and a visit to another screen, and a folder pressed in
// the detail pane's breadcrumb is shown in the tree (ADR-073 Inc.3).
//
// Why Tier1: which row is remembered and where it goes back to is decided in `nodeTreeReveal.ts` and
// unit-tested there. What only a browser proves is the wiring: that `pagehide` and the unmount really
// write the point, that the next load really reads it once the rows arrive, and that the crumb
// really scrolls the tree.
//
// ⚠️ Every test starts from a tree scrolled far enough that the unscrolled tree would show other
// rows, so a green run cannot be the tree happening to sit at the top.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { type Json } from '../support/openapi';
import {
  CHILD_ID,
  CHILD_NAME,
  groupSummary,
  membersByGroup,
  NODE_ID,
  NODE_NAME,
  twoLevelGroups,
} from '../support/twoLevelTree';
import { runNodeId, ungroupedRunByGroup } from '../support/ungroupedRun';

type Page = import('@playwright/test').Page;

const ROW_H = 30;
const body = (page: Page) => page.locator('.ntree-body');
const scrollTopOf = (page: Page) => body(page).evaluate((el) => el.scrollTop);

/** Scroll the tree as a person would, to `px`, and wait for it to settle there. */
async function scrollTreeTo(page: Page, px: number) {
  await body(page).evaluate((el, to) => {
    el.scrollTop = to;
  }, px);
  await expect.poll(() => scrollTopOf(page)).toBe(px);
}

/** The id of the row under the pane's top edge. */
const topRowId = (page: Page) =>
  body(page).evaluate((el) => {
    const pane = el.getBoundingClientRect().top;
    const rows = [...el.querySelectorAll('[id^="ntree-"]')].map((r) => ({ id: r.id, box: r.getBoundingClientRect() }));
    return rows.find((r) => r.box.top <= pane + 1 && r.box.bottom > pane + 1)?.id ?? null;
  });

/** Whether the row sits wholly inside the tree's scroller. */
async function inView(page: Page, selector: string): Promise<boolean> {
  const row = page.locator(selector);
  if ((await row.count()) === 0) return false;
  const box = await row.boundingBox();
  const pane = await body(page).boundingBox();
  if (!box || !pane) return false;
  return box.y >= pane.y && box.y + box.height <= pane.y + pane.height;
}

/** Filter mode's page: the nodes whose name holds the term, from `pool`. */
const searchOver = (pool: Record<string, Json>[]) => (url: URL): Json => {
  const term = (url.searchParams.get('search') ?? '').toLowerCase();
  return {
    nodes: pool.filter((n) => String(n.name).toLowerCase().includes(term)),
    truncated: false,
    next_cursor: null,
  } as unknown as Json;
};

test.describe('a reload', () => {
  const run = (ungroupedRunByGroup(new URL('http://x/api/v1/nodes/by-group')) as unknown as {
    nodes: Record<string, Json>[];
  }).nodes;
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': [] as unknown as Json,
        '/api/v1/nodes/by-group': ungroupedRunByGroup,
        '/api/v1/fleet/group-summary': { groups: {} } as unknown as Json,
        '/api/v1/nodes': searchOver(run),
      },
    },
  });

  test('keeps the tree where it was', async ({ page }) => {
    await page.goto('/nodes');
    await expect(page.locator(`[id="ntree-n:${runNodeId(0)}"]`)).toHaveCount(1);
    // Row 0 is the Ungrouped header, so node i is row i + 1. Part-way into a row on purpose.
    const at = 31 * ROW_H + 12;
    await scrollTreeTo(page, at);

    await page.reload();
    await expect.poll(() => scrollTopOf(page), { message: 'the reload moved the tree' }).toBe(at);
    expect(await inView(page, `[id="ntree-n:${runNodeId(0)}"]`)).toBe(false);
  });

  test('keeps it under a search too', async ({ page }) => {
    await page.goto('/nodes');
    await page.getByRole('searchbox', { name: 'Search' }).fill('node-');
    await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('node-');
    await expect(page.locator(`[id="ntree-n:${runNodeId(0)}"]`)).toHaveCount(1);
    const at = 20 * ROW_H + 5;
    await scrollTreeTo(page, at);
    const top = await topRowId(page);
    expect(top).toMatch(/^ntree-n:/);

    await page.reload();
    await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBe('node-');
    await expect.poll(() => scrollTopOf(page), { message: 'the reload moved the tree' }).toBe(at);
    expect(await topRowId(page)).toBe(top);
  });

  test('keeps the tree where it was after a visit to another screen', async ({ page }) => {
    await page.goto('/nodes');
    await expect(page.locator(`[id="ntree-n:${runNodeId(0)}"]`)).toHaveCount(1);
    const at = 25 * ROW_H + 7;
    await scrollTreeTo(page, at);
    // Away and back inside the app: no reload, so only the unmount wrote the point.
    await page.locator('a[href="/dashboard"]').first().click();
    await expect(body(page)).toHaveCount(0);
    await page.goBack();
    await expect.poll(() => scrollTopOf(page), { message: 'coming back moved the tree' }).toBe(at);
  });
});

test.describe('a folder pressed in the breadcrumb', () => {
  /** The two-level tree's member folder, plus sixty ungrouped nodes below it — enough to scroll the
   *  folder rows out of the pane. */
  const byGroup = (url: URL): Json =>
    url.searchParams.get('groups') || url.searchParams.get('group')
      ? membersByGroup(url)
      : ungroupedRunByGroup(url);
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': twoLevelGroups(),
        '/api/v1/nodes/by-group': byGroup,
        '/api/v1/fleet/group-summary': groupSummary(),
        '/api/v1/nodes/{node_id}': () =>
          ({ ...(deviceNode(NODE_ID) as object), id: NODE_ID, name: NODE_NAME, group_id: CHILD_ID }) as Json,
      },
    },
  });

  test('scrolls the tree to that folder', async ({ page }) => {
    await page.goto(`/nodes?sel=node:${NODE_ID}`);
    const folder = `[id="ntree-g:${CHILD_ID}"]`;
    await expect(page.locator(folder)).toHaveCount(1);
    await expect(page.locator(`[id="ntree-n:${runNodeId(59)}"]`)).toHaveCount(0);
    await scrollTreeTo(page, await body(page).evaluate((el) => el.scrollHeight - el.clientHeight));
    expect(await inView(page, folder), 'precondition: the folder is scrolled away').toBe(false);

    await page.locator('.nd-eyebrow .nd-crumb').filter({ hasText: CHILD_NAME }).click();
    await expect.poll(() => new URL(page.url()).searchParams.get('sel')).toBe(`group:${CHILD_ID}`);
    await expect.poll(() => inView(page, folder), { message: 'the tree did not show the folder' }).toBe(true);
  });
});
