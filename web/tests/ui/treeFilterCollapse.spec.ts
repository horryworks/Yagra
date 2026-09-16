// SPDX-License-Identifier: AGPL-3.0-only
// Closing a folder while the inventory tree is filtered (ADR-053 Inc.11, reshaped by ADR-154).
//
// Why Tier1: the rules themselves (`pressTwisty`, `filterCollapsedFrom`, `flattenTree`'s
// `filterCollapsed`) are unit-tested, but WHICH state the ▼ writes and which set the rows read is
// decided in `NodeTree.tsx`, which Vitest never runs. That wiring has been wrong twice, in opposite
// directions: first the arrow wrote the saved layout while a filtered tree ignored it (pressing it did
// nothing on screen), then it wrote only a set that was thrown away when the filter was cleared (the
// folder closed under the filter was open again afterwards — the ADR-154 report).
//
// The search names the PARENT folder, which reveals the child and the child's member, so the arrow
// under test belongs to a folder that is itself inside the match. Each test browses first: the
// member is on screen before anything is typed, so a later "gone" can only be the arrow's doing.
//
// ⚠️ What this cannot see: surviving a reload. The harness re-seeds `yagra_prefs` on every
// navigation (`support/app.ts`), so the layout a test wrote is gone before the next page load looks
// for it. That half is `serverPrefs.test.ts` and the live boxes.
// ⚠️ Every press here now saves to `PUT /api/v1/preferences`. The mock answers it; the live e2e
// harness refuses every non-GET, so a Tier2 spec that presses a twisty will fail on the save.

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
const childRow = (page: Page) => page.locator('.ntree-grow').filter({ hasText: CHILD_NAME });
const childTwisty = (page: Page) => childRow(page).locator('.ntree-twisty');
const ungroupedHeader = (page: Page) =>
  page.locator('.ntree-body .ntree-row').filter({ hasText: 'Ungrouped' });

/** Wait until the typed term has actually reached the tree — the search is debounced.
 *
 *  🚨 **Not "two folder rows".** The first version waited for that, and the unfiltered tree has
 *  exactly two folder rows too, so the wait passed at once: the arrow was pressed while the tree
 *  was still browsing. The Ungrouped header is what only browsing draws (with nothing ungrouped
 *  matching, a filtered tree has none). */
async function filterApplied(page: Page) {
  await expect(ungroupedHeader(page)).toHaveCount(0);
}

/** Clear the box and wait until the tree is browsing again — the header coming BACK is the
 *  transition, so an assertion after it is about the browse tree and not the filtered one it
 *  replaced. */
async function clearSearch(page: Page) {
  await page.locator('.nodes-pane-search').getByRole('button', { name: 'Clear search' }).click();
  await expect(ungroupedHeader(page)).toHaveCount(1);
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
  await expect(childRow(page)).toHaveCount(1);

  await childTwisty(page).click();
  await expect(member(page), 'the arrow did not open the folder again').toHaveCount(1);
});

test('a folder closed under a filter is still closed after the filter is cleared', async ({
  page,
}) => {
  // The ADR-154 report. Before it, the press went to a set the tree dropped with the filter.
  await page.goto('/nodes');
  await expect(member(page)).toHaveCount(1);

  await searchBox(page).fill(PARENT_NAME);
  await filterApplied(page);
  await childTwisty(page).click();
  await expect(member(page)).toHaveCount(0);

  await clearSearch(page);
  await expect(childRow(page)).toHaveCount(1);
  await expect(
    member(page),
    'clearing the filter reopened the folder closed under it',
  ).toHaveCount(0);
  await expect(childTwisty(page)).not.toHaveClass(/\bopen\b/);
});

test('a folder closed while browsing shows open under a filter, and opening it there sticks', async ({
  page,
}) => {
  await page.goto('/nodes');
  await expect(member(page)).toHaveCount(1);
  await childTwisty(page).click();
  await expect(member(page)).toHaveCount(0);

  // Inc.6: a folder closed while browsing must not hide a match.
  await searchBox(page).fill(PARENT_NAME);
  await filterApplied(page);
  await expect(member(page), 'the saved layout hid a match under the filter').toHaveCount(1);

  // One press closes what the row shows open — a flip would have opened it in the saved layout.
  await childTwisty(page).click();
  await expect(member(page)).toHaveCount(0);
  await childTwisty(page).click();
  await expect(member(page)).toHaveCount(1);

  await clearSearch(page);
  await expect(member(page), 'the folder last left open under the filter came back closed').toHaveCount(1);
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
