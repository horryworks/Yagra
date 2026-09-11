// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree fetches the folders it is SHOWING, not every folder that is open (ADR-125).
//
// The defect this pins was reported from a deployment with 500+ folders: opening Nodes took over
// ten seconds and sometimes never finished. The tree's rendering has been virtualized since S13,
// but its FETCHING was not — `visibleOpenGroupKeys` meant "no collapsed ancestor", and collapse
// state defaults to empty, so every folder counted. One `/nodes/by-group` per folder, all at once.
//
// 🚨 **Only a browser can see this.** The unit tests hand the hook its own `visibleGroupKeys`, so
// they prove what it does with the set — not that the set is the one the operator can see. What
// decides that is the virtualizer, the row heights, the pane's real height and the flattened row
// list, and none of those exist outside a rendered page.
//
// ⚠️ The generated mock answers `/api/v1/node-groups` with one folder, which cannot tell "fetched
// what is on screen" from "fetched everything". The overrides below are local to this file rather
// than in `BOOTSTRAP_OVERRIDES`, because thirty-odd other specs count on that one folder.

import { expect, test } from '../support/app';
import type { Route } from '@playwright/test';

/** Enough folders that a screenful is a small fraction of them. */
const FOLDER_COUNT = 40;

const folders = Array.from({ length: FOLDER_COUNT }, (_, i) => ({
  id: `00000000-0000-4000-8000-00000000f${String(i).padStart(3, '0')}`,
  name: `Folder ${String(i).padStart(2, '0')}`,
  group_type: 'generic',
  parent_id: null,
  sort_order: i,
  latitude: null,
  longitude: null,
  effective_latitude: null,
  effective_longitude: null,
  geo_group: null,
  geo_source: 'unset',
  pool: null,
  prefixes: [],
}));

/** Every folder holds members, so every folder wants a fetch. A folder the server reports as empty
 *  gets no placeholder row and would drop out of the fetch set for the wrong reason. */
const groupSummary = {
  groups: Object.fromEntries(
    folders.map((g) => [
      g.id,
      { ok: 5, warning: 0, critical: 0, unreachable: 0, maintenance: 0, unknown: 0 },
    ]),
  ),
};

const json = (body: unknown) => (route: Route) =>
  route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

test('fetches the folders on screen, not every folder that is open', async ({ page }) => {
  await page.route('**/api/v1/node-groups', json(folders));
  await page.route('**/api/v1/fleet/group-summary', json(groupSummary));

  /** Which folders were actually asked for. Counted from the wire, so the assertion is about the
   *  requests the operator's browser makes — the thing that was overwhelming the server. */
  const asked: string[] = [];
  const requests: URL[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname !== '/api/v1/nodes/by-group') return;
    requests.push(u);
    const batch = u.searchParams.get('groups');
    // Count FOLDERS, not requests: the batch form names several in one call, and the question this
    // file asks is which folders the tree went after — not how it packaged them.
    if (batch) asked.push(...batch.split(',').filter(Boolean));
    else asked.push(u.searchParams.get('group') ?? 'ungrouped');
  });

  await page.goto('/nodes');
  await expect(page.locator('.ntree-row.ntree-grow').first()).toBeVisible();
  // Past the viewport settle (PENDING_SETTLE_MS) with room to spare, so this measures the resting
  // state rather than a moment inside it.
  await page.waitForTimeout(600);

  // 🚨 **Both bounds, and the lower one is not padding.** A selector that stops matching, or a
  // tree that renders nothing, would make "0 requests" look like a perfect pass — which is the
  // failure this shape exists to keep distinguishable from success.
  expect(asked.length).toBeGreaterThan(0);
  expect(asked.length).toBeLessThan(FOLDER_COUNT);

  // 🚨 **Folders and requests are different numbers now, and that gap IS the batch form.** Measured
  // here: ~30 folders asked about across 6 requests. Before ADR-125 the two were equal by
  // construction — one request per folder, 41 of each. Asserting the relation rather than a
  // constant keeps this honest at other window sizes, where both numbers move.
  expect(requests.length).toBeLessThan(asked.length);

  // The regression in its own terms: before ADR-125 this was every folder plus the ungrouped
  // bucket, on the first paint, with nothing bounding how many went out at once.
  expect(asked.length).not.toBe(FOLDER_COUNT + 1);

  // Nothing is asked for twice — the queue de-duplicates within a render, and the three effects
  // that can all want the same folder no longer race into a second request for it.
  expect(new Set(asked).size).toBe(asked.length);

  // The ungrouped bucket is fetched unconditionally: it has no folder row, so the viewport can
  // never name it, and its header counts from what is loaded rather than from the server rollup.
  expect(asked).toContain('ungrouped');
});

test('asks for more folders as they are scrolled into view', async ({ page }) => {
  await page.route('**/api/v1/node-groups', json(folders));
  await page.route('**/api/v1/fleet/group-summary', json(groupSummary));

  const asked: string[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname !== '/api/v1/nodes/by-group') return;
    const batch = u.searchParams.get('groups');
    // Count FOLDERS, not requests: the batch form names several in one call, and the question this
    // file asks is which folders the tree went after — not how it packaged them.
    if (batch) asked.push(...batch.split(',').filter(Boolean));
    else asked.push(u.searchParams.get('group') ?? 'ungrouped');
  });

  await page.goto('/nodes');
  await expect(page.locator('.ntree-row.ntree-grow').first()).toBeVisible();
  await page.waitForTimeout(600);
  const afterFirstPaint = asked.length;

  await page.locator('.ntree-body').evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });
  await page.waitForTimeout(600);

  // The other half of the contract: following the viewport means the folders further down are
  // fetched when they arrive, not never. Without this the first assertion is satisfied by a tree
  // that fetches nothing at all after the first screenful.
  expect(asked.length).toBeGreaterThan(afterFirstPaint);
  expect(new Set(asked).size).toBe(asked.length);
});

test('paints the folders and asks for members before the per-group rollup answers', async ({
  page,
}) => {
  // ADR-133. The tree needs the folder list and nothing else to draw itself, and the member fetch
  // is gated on the same `loading` flag — so waiting for all three skeleton calls made the first
  // member request wait on the slowest of them. `/fleet/group-summary` is the slowest by
  // construction: it scans the whole `nodes` table and, whenever the alert engine has no opinion
  // about someone, runs a fleet-wide TSDB freshness query as well.
  //
  // 🚨 **Only a browser can see this.** The unit tests prove `flattenTree` emits the placeholder
  // rows that become the fetch set; what they cannot prove is that the page reaches that state
  // while one of its three requests is still in flight. That is React effect ordering, the
  // virtualizer, and the settle timer — none of which exist outside a rendered page.
  const ROLLUP_DELAY_MS = 3000;
  await page.route('**/api/v1/node-groups', json(folders));
  await page.route('**/api/v1/fleet/group-summary', async (route) => {
    await new Promise((r) => setTimeout(r, ROLLUP_DELAY_MS));
    await json(groupSummary)(route);
  });

  let rollupAnsweredAt: number | null = null;
  page.on('response', (r) => {
    if (new URL(r.url()).pathname === '/api/v1/fleet/group-summary') rollupAnsweredAt = Date.now();
  });
  // 🚨 **Requests that NAME A FOLDER, never "any `/nodes/by-group` request".** The ungrouped bucket
  // is fetched unconditionally on first paint (it has no folder row, so the viewport can never name
  // it), so counting every by-group call makes this test pass with the fix reverted — measured: it
  // did, and the assertion looked perfectly reasonable while proving nothing.
  const folderAskedAt: number[] = [];
  page.on('request', (r) => {
    const u = new URL(r.url());
    if (u.pathname !== '/api/v1/nodes/by-group') return;
    if (u.searchParams.get('groups') || u.searchParams.get('group')) folderAskedAt.push(Date.now());
  });

  await page.goto('/nodes');
  // The folder rows are on screen…
  await expect(page.locator('.ntree-row.ntree-grow').first()).toBeVisible({ timeout: 2000 });
  // …and their members are already being fetched, while the rollup is still out.
  await expect.poll(() => folderAskedAt.length, { timeout: 2000 }).toBeGreaterThan(0);
  expect(rollupAnsweredAt).toBeNull();

  // ⚠️ **Assert the ORDER, not the elapsed time.** A 2s timeout against a 3s delay says the same
  // thing today and becomes flaky on a loaded runner; the relation is what the change is about.
  const firstFolderAsk = folderAskedAt[0];
  await page.waitForResponse((r) => new URL(r.url()).pathname === '/api/v1/fleet/group-summary');
  expect(rollupAnsweredAt).not.toBeNull();
  expect(firstFolderAsk).toBeLessThan(rollupAnsweredAt as unknown as number);

  // And the counts land afterwards rather than never: the pill fills in.
  await expect(page.locator('.ntree-row.ntree-grow .ntree-count').first()).toHaveText(/\d/, {
    timeout: 3000,
  });
});
