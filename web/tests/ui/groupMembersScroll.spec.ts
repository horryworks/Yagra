// SPDX-License-Identifier: AGPL-3.0-only
// A folder with more members than the pane is tall scrolls its body, so the last member can be
// reached.
//
// Why Tier1: it is a layout property, and only a browser lays anything out. `.nodes-pane` clips
// (`overflow: hidden`), so the pane's body has to be the scroller; the node detail's `.nd-body`
// always was, and the folder's `.nd-grpbody` shipped without it. Every folder that fit on one screen
// looked fine, and the default mock has three folders, so the walk never saw it — the first folder
// big enough was an imported Meraki organization with 131 networks.
//
// 🚨 **The scroll is a WHEEL over the body, never `scrollIntoView` and never Playwright's own
// actionability scroll.** A box with `overflow: hidden` is still a scroll container to script, so
// either of those scrolls the clipping pane itself and brings the last row into view against the
// shipped bug. The operator has a wheel and a scrollbar, and the wheel does nothing to a clipped box.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const PARENT_ID = '00000000-0000-4000-8000-00000000f000';
const SUBFOLDERS = 80;
const subName = (i: number) => `${MOCK_PREFIX}sub-${String(i).padStart(3, '0')}`;

/** One folder holding eighty, built from the generated row (testing.md: no hand-written copy). */
function folders(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const folder = (n: number, name: string, parent: string | null) => ({
    ...template,
    id: `00000000-0000-4000-8000-00000000f${String(n).padStart(3, '0')}`,
    name,
    parent_id: parent,
    group_type: 'generic',
    sort_order: n,
  });
  return [
    folder(0, `${MOCK_PREFIX}big-folder`, null),
    ...Array.from({ length: SUBFOLDERS }, (_, i) => folder(i + 1, subName(i + 1), PARENT_ID)),
  ] as unknown as Json;
}

test.use({
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/node-groups': folders() } },
});

test("a folder's member list scrolls inside the pane, down to its last member", async ({
  page,
  errors,
}) => {
  await page.goto(`/nodes?sel=group:${PARENT_ID}`);
  const pane = page.locator('.nodes-detail-pane');
  const body = page.locator('.nd-grpbody');
  const members = body.locator('.nd-member');
  const last = members.filter({ hasText: subName(SUBFOLDERS) });
  await expect(members.first()).toBeVisible();
  await expect(members).toHaveCount(SUBFOLDERS);

  // The precondition, asserted: against a list that fits, "the last row is inside the pane" holds
  // with or without a scroller (`floor-must-count-what-was-checked`).
  const paneBox = (await pane.boundingBox())!;
  const bottom = paneBox.y + paneBox.height;
  expect((await last.boundingBox())!.y, 'the list fits the pane — this fixture cannot see the bug')
    .toBeGreaterThan(bottom);

  const headBefore = (await page.locator('.nd-head').boundingBox())!.y;
  const bodyBox = (await body.boundingBox())!;
  await page.mouse.move(bodyBox.x + bodyBox.width / 2, bodyBox.y + 40);
  for (let i = 0; i < 12; i++) await page.mouse.wheel(0, 600);

  await expect
    .poll(async () => {
      const b = await last.boundingBox();
      return b ? b.y + b.height : Number.POSITIVE_INFINITY;
    }, { message: 'the last member never came into the pane' })
    .toBeLessThanOrEqual(bottom);

  // Only the body moved: the header — the folder's name, counts and actions — stays put, as it does
  // over a node's tabs.
  expect((await page.locator('.nd-head').boundingBox())!.y).toBeCloseTo(headBefore, 0);
  expect(await pane.evaluate((el) => el.scrollTop), 'the clipping pane itself was scrolled').toBe(0);

  expect(errors.uncaught).toEqual([]);
});
