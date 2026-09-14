// SPDX-License-Identifier: AGPL-3.0-only
// The detail panes open what they name (ADR-142): a folder on a node's breadcrumb, an ancestor on a
// folder's title, a row under a folder's Members.
//
// Why Tier1: the pure half (`groupTrail`, `nodesPageHref`) is unit-tested, but whether a click on a
// segment reaches `select()` — and whether the pane that replaces this one is the right one — is
// wiring, and only a browser runs it.
//
// The fixture is two folders deep on purpose. The bootstrap mock flattens every folder to the root
// (`bootstrap.ts`), and a one-level path has no ancestor, so the "last segment is not a link" half of
// the group title could not go wrong there.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { type Json } from '../support/openapi';
import {
  CHILD_ID,
  CHILD_NAME,
  membersByGroup,
  NODE_ID,
  NODE_NAME,
  PARENT_ID,
  PARENT_NAME,
  twoLevelGroups,
} from '../support/twoLevelTree';

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': twoLevelGroups(),
      '/api/v1/nodes/by-group': membersByGroup,
      '/api/v1/nodes/{node_id}': () =>
        ({ ...(deviceNode(NODE_ID) as object), id: NODE_ID, name: NODE_NAME, group_id: CHILD_ID }) as Json,
    },
  },
});

/** The selection lives in `?sel=`; read it decoded rather than matching the encoded colon. */
const selected = (page: { url(): string }) => new URL(page.url()).searchParams.get('sel');

test("a folder on a node's breadcrumb opens that folder in the pane", async ({ page }) => {
  await page.goto(`/nodes?sel=node:${NODE_ID}`);
  const crumbs = page.locator('.nd-eyebrow .nd-crumb');
  // Both folders are links on a node: the last one is the node's parent, not what is open.
  await expect(crumbs).toHaveCount(2);

  await crumbs.filter({ hasText: PARENT_NAME }).click();
  await expect.poll(() => selected(page)).toBe(`group:${PARENT_ID}`);
  await expect(page.locator('.nd-grpbody')).toBeVisible();
  await expect(page.locator('.nd-name')).toHaveText(PARENT_NAME);
});

test("a folder's title links its ancestors but not itself, and a member row opens the node", async ({
  page,
}) => {
  await page.goto(`/nodes?sel=group:${CHILD_ID}`);
  const title = page.locator('.nd-name');
  await expect(title).toContainText(CHILD_NAME);

  // The last segment IS this pane; a link to it would do nothing. Asserted as "the ancestor is a
  // button AND the last is not", so a title with no buttons at all cannot pass the second half.
  await expect(title.locator('.nd-crumb')).toHaveCount(1);
  await expect(title.locator('.nd-crumb')).toHaveText(PARENT_NAME);
  await expect(title.getByRole('button', { name: CHILD_NAME })).toHaveCount(0);

  const member = page.locator('.nd-member-link').filter({ hasText: NODE_NAME });
  await expect(member).toBeVisible();
  await member.click();
  await expect.poll(() => selected(page)).toBe(`node:${NODE_ID}`);
  await expect(page.locator('.nd-name')).toHaveText(NODE_NAME);
});

test("a folder's Members lists its subfolder, and pressing it opens that folder", async ({ page }) => {
  await page.goto(`/nodes?sel=group:${PARENT_ID}`);
  const sub = page.locator('.nd-members .nd-member-group');
  await expect(sub).toHaveCount(1);
  await expect(sub.locator('.nd-member-name')).toHaveText(CHILD_NAME);

  await sub.click();
  await expect.poll(() => selected(page)).toBe(`group:${CHILD_ID}`);
  await expect(page.locator('.nd-name')).toContainText(CHILD_NAME);
  // The pane really moved: the child has no subfolder of its own, and its node is listed. Asserted
  // after the positive half so the "no subfolder row" count is read from the settled child pane.
  await expect(page.locator('.nd-member-link').filter({ hasText: NODE_NAME })).toBeVisible();
  await expect(page.locator('.nd-member-group')).toHaveCount(0);
});

test('on the standalone node page a folder takes you to All nodes with it open', async ({ page }) => {
  await page.goto(`/nodes/${NODE_ID}`);
  const crumb = page.locator('.nd-eyebrow .nd-crumb').filter({ hasText: CHILD_NAME });
  await expect(crumb).toBeVisible();

  await crumb.click();
  await expect.poll(() => new URL(page.url()).pathname).toBe('/nodes');
  expect(selected(page)).toBe(`group:${CHILD_ID}`);
  await expect(page.locator('.nd-grpbody')).toBeVisible();
});
