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
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const PARENT_ID = '00000000-0000-4000-8000-0000000000c1';
const CHILD_ID = '00000000-0000-4000-8000-0000000000c2';
const NODE_ID = '00000000-0000-4000-8000-0000000000c3';
const PARENT_NAME = `${MOCK_PREFIX}region`;
const CHILD_NAME = `${MOCK_PREFIX}site`;
const NODE_NAME = `${MOCK_PREFIX}member`;

/** Parent → child, built from the generated group row so a change to its shape reaches this too. */
function twoLevelGroups(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  return [
    { ...template, id: PARENT_ID, name: PARENT_NAME, parent_id: null, group_type: 'generic' },
    { ...template, id: CHILD_ID, name: CHILD_NAME, parent_id: PARENT_ID, group_type: 'generic' },
  ] as unknown as Json;
}

/** One node, filed in the child folder. Mirrors the two forms `bootstrap.ts` answers: the batch
 *  form echoes the folders it was asked about in `answered` (without it the tree re-queues the
 *  folder forever, ADR-125), and the single-group form carries no echo. */
function membersByGroup(url: URL): Json {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as {
    nodes: Record<string, Json>[];
    answered?: string[];
  };
  const member = { ...body.nodes[0], id: NODE_ID, name: NODE_NAME, group_id: CHILD_ID, sort_order: 1 };
  const batch = url.searchParams.get('groups');
  if (batch) {
    const asked = batch.split(',').filter(Boolean);
    return {
      nodes: asked.includes(CHILD_ID) ? [member] : [],
      truncated: false,
      answered: asked,
    } as unknown as Json;
  }
  delete body.answered;
  const nodes = url.searchParams.get('group') === CHILD_ID ? [member] : [];
  return { ...body, nodes } as unknown as Json;
}

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

test('on the standalone node page a folder takes you to All nodes with it open', async ({ page }) => {
  await page.goto(`/nodes/${NODE_ID}`);
  const crumb = page.locator('.nd-eyebrow .nd-crumb').filter({ hasText: CHILD_NAME });
  await expect(crumb).toBeVisible();

  await crumb.click();
  await expect.poll(() => new URL(page.url()).pathname).toBe('/nodes');
  expect(selected(page)).toBe(`group:${CHILD_ID}`);
  await expect(page.locator('.nd-grpbody')).toBeVisible();
});
