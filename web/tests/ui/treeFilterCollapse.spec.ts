// SPDX-License-Identifier: AGPL-3.0-only
// Closing a folder while the inventory tree is filtered (ADR-053 Inc.11).
//
// Why Tier1: the rule itself (`flattenTree`'s `filterCollapsed`, `treeFilterKey`) is unit-tested, but
// WHICH set the ▼ writes is decided in `NodeTree.tsx`, which Vitest never runs. That wiring was the
// bug: the arrow wrote the saved layout while a filtered tree ignored it, so pressing it did nothing
// on screen — and collapsed the folder in the tree the operator came back to.
//
// The search names the PARENT folder, which reveals the child and the child's member, so the arrow
// under test belongs to a folder that is itself inside the match. Each test browses first: the
// member is on screen before anything is typed, so a later "gone" can only be the arrow's doing.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import {
  CHILD_NAME,
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
const childTwisty = (page: Page) =>
  page.locator('.ntree-grow').filter({ hasText: CHILD_NAME }).locator('.ntree-twisty');

/** Wait until the typed term has actually reached the tree — the search is debounced.
 *
 *  🚨 **Not "two folder rows".** The first version waited for that, and the unfiltered tree has
 *  exactly two folder rows too, so the wait passed at once: the arrow was pressed while the tree
 *  was still browsing, wrote the saved layout, and the tests failed in a way that looked like the
 *  fix writing the wrong set. The Ungrouped header is what only browsing draws (with nothing
 *  ungrouped matching, a filtered tree has none). */
async function filterApplied(page: Page) {
  await expect(page.locator('.ntree-body .ntree-row').filter({ hasText: 'Ungrouped' })).toHaveCount(0);
}

test('a folder closes and opens while the tree is filtered', async ({ page }) => {
  await page.goto('/nodes');
  await expect(member(page)).toHaveCount(1);

  await searchBox(page).fill(PARENT_NAME);
  // The parent, the child and the member — and nothing ungrouped, so the filter has been applied.
  await filterApplied(page);
  await expect(member(page)).toHaveCount(1);

  await childTwisty(page).click();
  await expect(member(page), 'the arrow did not close the folder while filtered').toHaveCount(0);
  await expect(page.locator('.ntree-grow').filter({ hasText: CHILD_NAME })).toHaveCount(1);

  await childTwisty(page).click();
  await expect(member(page), 'the arrow did not open the folder again').toHaveCount(1);
});

test('closing a folder under a filter does not change the unfiltered tree', async ({ page }) => {
  await page.goto('/nodes');
  await expect(member(page)).toHaveCount(1);

  await searchBox(page).fill(PARENT_NAME);
  await filterApplied(page);
  await childTwisty(page).click();
  await expect(member(page)).toHaveCount(0);

  await page.locator('.nodes-pane-search').getByRole('button', { name: 'Clear search' }).click();
  await expect(
    member(page),
    'the folder closed under the filter is closed in the saved layout too',
  ).toHaveCount(1);
});

test('a different filter starts with every folder open', async ({ page }) => {
  await page.goto('/nodes');
  await expect(member(page)).toHaveCount(1);

  await searchBox(page).fill(PARENT_NAME);
  await filterApplied(page);
  await childTwisty(page).click();
  await expect(member(page)).toHaveCount(0);

  // Still matches the parent, but it is a new question: a folder closed under the last one must
  // not hide this one's match.
  await searchBox(page).fill('region');
  await expect(member(page), 'the new filter inherited the old one\'s closed folder').toHaveCount(1);
});
