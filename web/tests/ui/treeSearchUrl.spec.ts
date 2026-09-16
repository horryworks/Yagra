// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's search term survives a reload (ADR-153).
//
// Why Tier1: the hook that holds the term (`useUrlTerm`) and the value it commits
// (`useFilterSearch().settledTerm`) are unit-tested, but the wiring between them — which effect
// commits, and that `clearAllFilters` folds the term into its single URL write — lives in
// `NodesPage.tsx`, which Vitest never runs. The failure this exists for is the one the operator
// reported: type "TDC", press F5, and the tree is unfiltered with an empty box.
//
// "Reload" here is `page.goto(page.url())`: a fresh document from the URL the screen itself wrote.
// That is the whole of what survives an F5 on this path, since the harness re-seeds localStorage on
// every navigation (`tests/support/app.ts`) — which is fine, because the term must not depend on it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import {
  groupSummary,
  membersByGroup,
  NODE_NAME,
  PARENT_NAME,
  twoLevelGroups,
} from '../support/twoLevelTree';

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': twoLevelGroups(),
      '/api/v1/nodes/by-group': membersByGroup,
      '/api/v1/fleet/group-summary': groupSummary(),
    },
  },
});

type Page = import('@playwright/test').Page;

const searchBox = (page: Page) => page.getByRole('searchbox', { name: 'Search' });
const member = (page: Page) => page.locator('.ntree-body').getByText(NODE_NAME, { exact: true });

/** The filter has reached the tree. Not "a folder row is drawn" — the unfiltered tree draws the same
 *  folders; the Ungrouped header is what only browsing draws (`treeFilterCollapse.spec.ts`). */
async function filterApplied(page: Page) {
  await expect(page.locator('.ntree-body .ntree-row').filter({ hasText: 'Ungrouped' })).toHaveCount(0);
}

const termInUrl = new RegExp(`[?&]q=${encodeURIComponent(PARENT_NAME)}(&|$)`);

test('a typed term is still in the box, and still filtering, after a reload', async ({ page }) => {
  await page.goto('/nodes');
  await expect(member(page)).toHaveCount(1);

  await searchBox(page).fill(PARENT_NAME);
  await filterApplied(page);
  await expect(page, 'the settled term never reached the URL').toHaveURL(termInUrl);

  await page.goto(page.url());
  await expect(searchBox(page), 'the reload emptied the box').toHaveValue(PARENT_NAME);
  await filterApplied(page);
  await expect(member(page), 'the reloaded tree is not filtered by the term').toHaveCount(1);
  await expect(page, 'loading the page took the term back out of the URL').toHaveURL(termInUrl);
});

test('clear all filters removes the term and the other filters in one go', async ({ page }) => {
  await page.goto('/nodes?state=ok');
  await searchBox(page).fill(PARENT_NAME);
  await expect(page).toHaveURL(termInUrl);
  await expect(page).toHaveURL(/[?&]state=ok/);

  await page.getByRole('button', { name: /Clear all filters/ }).click();
  await expect(searchBox(page)).toHaveValue('');
  // Both, checked after the settle has had its chance: a second write built from the pre-clear
  // snapshot would put `state=ok` back a moment later, and a check made at once would miss it.
  await page.waitForTimeout(400);
  const params = new URL(page.url()).searchParams;
  expect(params.get('q'), 'clear all left the term in the URL').toBeNull();
  expect(params.get('state'), 'the term\'s settle restored the state filter clear all removed').toBeNull();
});
