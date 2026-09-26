// SPDX-License-Identifier: AGPL-3.0-only
// A node picked from a narrowed tree is still on screen after the filter is cleared (ADR-073 増分 2).
//
// Why Tier1: which folders a reveal opens and when it gives up is decided in `nodeTreeReveal.ts` and
// unit-tested there. What only a browser proves is the wiring the report was about — the folder is
// loaded although it was never on screen, the saved layout really opens, and the tree really scrolls.
// Before this, `?sel=` survived the clear while the row did not, which read as a lost selection.
//
// ⚠️ Each test starts from the state that hid the row in the report, so a green run cannot be the
// tree happening to show it anyway: the folder closed in the saved layout (so never loaded while
// browsing), and a node far enough down the list that the unscrolled tree cannot show it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import {
  CHILD_ID,
  CHILD_NAME,
  groupSummary,
  membersByGroup,
  NODE_ID,
  NODE_NAME,
  twoLevelGroups,
} from '../support/twoLevelTree';
import { runNodeId, runNodeName, ungroupedRunByGroup } from '../support/ungroupedRun';

type Page = import('@playwright/test').Page;

const selected = (page: Page) => new URL(page.url()).searchParams.get('sel');
const searchBox = (page: Page) => page.getByRole('searchbox', { name: 'Search' });
const rowById = (page: Page, id: string) => page.locator(`[id="ntree-n:${id}"]`);
const ungroupedHeader = (page: Page) =>
  page.locator('.ntree-body .ntree-row').filter({ hasText: 'Ungrouped' });

/** Clear the box with its ✕ and wait until the term has left the URL.
 *
 *  ⚠️ Not "the Ungrouped header is back", which is how `treeFilterCollapse` waits: on the long list
 *  the header is row 0, and a reveal that works scrolls it out of the virtualizer's window — so that
 *  wait fails precisely when the feature does its job. */
async function clearSearch(page: Page) {
  await page.locator('.nodes-pane-search').getByRole('button', { name: 'Clear search' }).click();
  await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBeNull();
}

/** Whether the row sits wholly inside the tree's scroller. A row the virtualizer has not drawn is
 *  not in view — and `boundingBox` would wait for it forever, so it is counted first. */
async function rowInView(page: Page, id: string): Promise<boolean> {
  if ((await rowById(page, id).count()) === 0) return false;
  const row = await rowById(page, id).boundingBox();
  const pane = await page.locator('.ntree-body').boundingBox();
  if (!row || !pane) return false;
  return row.y >= pane.y && row.y + row.height <= pane.y + pane.height;
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

test.describe('a node inside a folder the saved layout keeps closed', () => {
  const member = (() => {
    const body = defaultBodyFor('/api/v1/nodes/by-group') as { nodes: Record<string, Json>[] };
    return { ...body.nodes[0], id: NODE_ID, name: NODE_NAME, group_id: CHILD_ID, sort_order: 1 };
  })();
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': twoLevelGroups(),
        '/api/v1/nodes/by-group': membersByGroup,
        '/api/v1/fleet/group-summary': groupSummary(),
        '/api/v1/nodes': searchOver([member]),
        // The account's layout: the folder holding the node is closed, so browsing never loads it.
        '/api/v1/preferences': (_url, request) =>
          request.method() === 'PUT'
            ? ({ ok: true } as unknown as Json)
            : ({ nodeTreeCollapsed: { [CHILD_ID]: true } } as unknown as Json),
      },
    },
  });

  test('clearing the filter opens the folder and keeps the node selected', async ({ page }) => {
    await page.goto('/nodes');
    const childRow = page.locator('.ntree-grow').filter({ hasText: CHILD_NAME });
    await expect(childRow).toHaveCount(1);
    await expect(childRow.locator('.ntree-twisty')).not.toHaveClass(/\bopen\b/);
    await expect(rowById(page, NODE_ID)).toHaveCount(0);

    await searchBox(page).fill(NODE_NAME);
    await expect(ungroupedHeader(page)).toHaveCount(0);
    await page.locator('.ntree-body').getByText(NODE_NAME, { exact: true }).click();
    await expect.poll(() => selected(page)).toBe(`node:${NODE_ID}`);

    await clearSearch(page);
    // Browsing again (the short tree keeps the header on screen) — before this the node's row could
    // still be the search page's, which was never the one that went missing.
    await expect(ungroupedHeader(page)).toHaveCount(1);
    await expect(rowById(page, NODE_ID), 'the node row did not come back').toHaveCount(1);
    await expect(rowById(page, NODE_ID)).toHaveClass(/\bsel\b/);
    await expect(childRow.locator('.ntree-twisty')).toHaveClass(/\bopen\b/);
    expect(selected(page), 'clearing the filter dropped the selection').toBe(`node:${NODE_ID}`);
  });
});

test.describe('a node far down a long list', () => {
  const TARGET = 55;
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

  test('clearing the filter scrolls the tree to the selected node', async ({ page }) => {
    await page.goto('/nodes');
    await expect(rowById(page, runNodeId(0))).toHaveCount(1);
    // The precondition: unscrolled, the tree cannot show the target at all.
    expect(await rowInView(page, runNodeId(TARGET))).toBe(false);

    await searchBox(page).fill(runNodeName(TARGET));
    await expect(rowById(page, runNodeId(0))).toHaveCount(0);
    await page.locator('.ntree-body').getByText(runNodeName(TARGET), { exact: true }).click();
    await expect.poll(() => selected(page)).toBe(`node:${runNodeId(TARGET)}`);

    await clearSearch(page);
    await expect
      .poll(() => rowInView(page, runNodeId(TARGET)), { message: 'the tree did not scroll to the node' })
      .toBe(true);
    await expect(rowById(page, runNodeId(TARGET))).toHaveClass(/\bsel\b/);
    expect(selected(page)).toBe(`node:${runNodeId(TARGET)}`);
  });

  test('clearing a filter while another still narrows the tree scrolls nothing', async ({ page }) => {
    // The one-of-two case: the reveal waits for the LAST narrowing control, so a tree the operator
    // is still narrowing does not move under them.
    await page.goto(`/nodes?state=ok`);
    await searchBox(page).fill(runNodeName(TARGET));
    await page.locator('.ntree-body').getByText(runNodeName(TARGET), { exact: true }).click();
    await expect.poll(() => selected(page)).toBe(`node:${runNodeId(TARGET)}`);
    await page.locator('.nodes-pane-search').getByRole('button', { name: 'Clear search' }).click();
    await expect.poll(() => new URL(page.url()).searchParams.get('q')).toBeNull();
    // Still narrowed by state, so the tree shows the search page for `state=ok` — every node — and
    // must not have been scrolled for the operator.
    await expect(rowById(page, runNodeId(0))).toHaveCount(1);
    expect(await page.locator('.ntree-body').evaluate((el) => el.scrollTop)).toBe(0);
  });
});
