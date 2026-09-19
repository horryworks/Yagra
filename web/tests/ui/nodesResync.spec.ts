// SPDX-License-Identifier: AGPL-3.0-only
// All nodes when the live stream reconnects (ADR-133 増分 7 決定 4, ADR-052 Tier1).
//
// A reconnect means state frames were missed, so the page re-reads what it shows. It first did that
// with the reload it uses after a write, which EMPTIES the member cache: for a round trip every
// loaded node row was gone, the tree flashed "Loading…", and an arrow key pressed in that window was
// dropped — a cursor whose row is not on screen is cleared rather than written (`settleCursor`).
// Found as a Tier1 flake: `treeKeyboard.spec.ts` failed only under the load of the full walk.
//
// ⚠️ **The reconnect here is the harness's, and this spec depends on it.** The mock ends every
// stream at once (`tests/support/mockApi.ts`), so the client reconnects every `RECONNECT_MS` and
// each reconnect after the first is a resync (`services/sse.ts`). If that ever changes, the
// `waitForRequest` below times out — a loud failure, never a vacuous pass.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, TREE_SIBLING_IDS } from '../support/bootstrap';

type Page = import('@playwright/test').Page;

const selected = (page: Page) => new URL(page.url()).searchParams.get('sel');
const rowById = (page: Page, id: string) => page.locator(`[id="ntree-n:${id}"]`);

const STREAM = '/api/v1/stream/node-states';
const MEMBERS = '/api/v1/nodes/by-group';
const FOLDERS = '/api/v1/node-groups';

test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

test('a reconnect re-reads the rows in place: they stay on screen, and a key pressed meanwhile lands', async ({
  page,
}) => {
  // Every request, in the order the page made it. Polled rather than awaited one by one: the re-read
  // follows the reconnect within a frame, and a second `waitForRequest` registered after the first
  // resolved can miss it.
  const seen: string[] = [];
  page.on('request', (r) => seen.push(new URL(r.url()).pathname));
  // Hold every member read, so the re-read a reconnect starts stays in flight long enough to act in.
  await page.route('**/api/v1/nodes/by-group**', async (route) => {
    await new Promise((r) => setTimeout(r, 1500));
    await route.fallback();
  });
  await page.goto('/nodes');
  const [first, second] = TREE_SIBLING_IDS;
  await expect(rowById(page, second)).toBeVisible();
  await rowById(page, first).click();
  await expect.poll(() => selected(page)).toBe(`node:${first}`);

  // From here on the page is settled: the next member read is the one a reconnect started.
  const mark = seen.length;
  const resync = () => {
    const after = seen.slice(mark);
    const reconnect = after.indexOf(STREAM);
    const reread = reconnect < 0 ? -1 : after.indexOf(MEMBERS, reconnect);
    return reread < 0 ? null : after.slice(reconnect, reread + 1);
  };
  await expect.poll(resync, { timeout: 10_000, message: 'no reconnect re-read within 10 s' }).not.toBeNull();

  // Read at once, not waited for: a row that vanished and came back would satisfy a waiting check.
  expect(await rowById(page, first).isVisible(), 'the re-read emptied the tree').toBe(true);
  // What was missed is states — not folders. The reload a write uses reads the folder list FIRST,
  // then the members; the in-place re-read goes straight to the members.
  expect(resync(), 'the resync re-read the folder list').not.toContain(FOLDERS);

  await page.keyboard.press('ArrowDown');
  await expect
    .poll(() => selected(page), { message: 'the key pressed during the re-read was dropped' })
    .toBe(`node:${second}`);
});
