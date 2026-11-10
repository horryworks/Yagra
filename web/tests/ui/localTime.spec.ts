// SPDX-License-Identifier: AGPL-3.0-only
// A site's local time and delivery state at the end of a header's sub line (ADR-252): a folder with
// a zone shows its clock, a folder without one shows none, and a node shows its site's answer.
//
// Why Tier1: what the line says is `lib/localTime.ts`'s and unit-tested; whether both panes read
// the fields the API carries, and lay them on the sub line, is wiring that only a browser runs.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { type Json } from '../support/openapi';
import {
  CHILD_ID,
  membersByGroup,
  NODE_ID,
  NODE_NAME,
  PARENT_ID,
  twoLevelGroups,
} from '../support/twoLevelTree';

// Two hours ahead: far enough that the answer is not stale while the spec runs.
const LATER = new Date(Date.now() + 2 * 3_600_000).toISOString();

/** The generated rows carry no zone; the child is the site, in Tokyo, inside its hours. */
function zonedGroups(): Json {
  return (twoLevelGroups() as Record<string, Json>[]).map((g) =>
    g.id === CHILD_ID
      ? {
          ...g,
          time_zone: null,
          netbox_time_zone: 'Asia/Tokyo',
          effective_time_zone: 'Asia/Tokyo',
          delivery: { open: true, closes_at: LATER, opens_at: null },
        }
      : { ...g, time_zone: null, netbox_time_zone: null, effective_time_zone: null, delivery: null },
  ) as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': zonedGroups(),
      '/api/v1/nodes/by-group': membersByGroup,
      '/api/v1/nodes/{node_id}': () =>
        ({
          ...(deviceNode(NODE_ID) as object),
          id: NODE_ID,
          name: NODE_NAME,
          group_id: CHILD_ID,
          time_zone: 'Asia/Tokyo',
          delivery: { open: false, closes_at: null, opens_at: LATER },
        }) as Json,
    },
  },
});

test("a site's pane shows its clock and that notifications are delivered", async ({ page }) => {
  await page.goto(`/nodes?sel=group:${CHILD_ID}`);
  const sub = page.locator('.nd-sub');
  await expect(sub).toContainText(/Local time \d\d:\d\d Asia\/Tokyo \(UTC \+0900\)/);
  await expect(sub).toContainText(/Delivering until (\S+ )?\d\d:\d\d/);
});

test('a folder no zone reaches shows no clock and no delivery state', async ({ page }) => {
  await page.goto(`/nodes?sel=group:${PARENT_ID}`);
  const sub = page.locator('.nd-sub');
  // The sub line has drawn (its node count), so an absent clock is an answer, not a slow one.
  await expect(sub).toContainText(/\d+ node/);
  await expect(sub).not.toContainText('Asia/Tokyo');
  await expect(sub).not.toContainText(/Delivering|Held/);
});

test("a node's pane shows its site's clock and that notifications are held", async ({ page }) => {
  await page.goto(`/nodes?sel=node:${NODE_ID}`);
  const sub = page.locator('.nd-sub');
  await expect(sub).toContainText('Asia/Tokyo');
  await expect(sub).toContainText(/Held until (\S+ )?\d\d:\d\d/);
});
