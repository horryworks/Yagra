// SPDX-License-Identifier: AGPL-3.0-only
// A folder an integration keeps is marked in the inventory tree; one a person made is not
// (ADR-164 Inc.7).
//
// Why Tier1: which origin a folder is marked with — including none, for a token this build does
// not know — is `lib/groupOrigin.ts`, unit-tested. What Vitest cannot run is the row in
// `NodeTree.tsx` that draws it. And the walk cannot stand in: `bootstrap.ts` serves folders with no
// origin on purpose, so deleting the badge from the tree would leave every walked screen green.
//
// The folders are the generated row, patched — a hand-written one would be a second copy of the
// contract (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const label = (s: string) => `${MOCK_PREFIX}${s}`;

function folders(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const folder = (n: number, name: string, origin: string | null) => ({
    ...template,
    id: `00000000-0000-4000-8000-0000000000e${n}`,
    name: label(name),
    parent_id: null,
    group_type: 'generic',
    sort_order: n,
    origin,
  });
  return [
    folder(1, 'from-netbox', 'netbox'),
    folder(2, 'from-meraki', 'meraki'),
    folder(3, 'by-hand', null),
  ] as unknown as Json;
}

test.use({
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/node-groups': folders() } },
});

test('a folder an integration keeps carries its badge, and a hand-made one carries none', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes');
  const row = (name: string) => page.locator('.ntree-grow').filter({ hasText: label(name) });

  // In the tree the badge is the brand's one letter (2026-09-29); the name is its tooltip and its
  // accessible name, and the folder's own header spells it out (checked below).
  const netbox = row('from-netbox').locator('.ntree-badge');
  await expect(netbox).toHaveText('N');
  await expect(netbox).toHaveAttribute('title', 'Created by the NetBox integration');
  await expect(netbox).toHaveAttribute('aria-label', 'Created by the NetBox integration');
  // Each brand's ink, on a faint tint of itself rather than a filled pill: a filled pill is what
  // the tree was too loud with.
  const colours = (badge: typeof netbox) =>
    badge.evaluate((el) => {
      const cs = getComputedStyle(el);
      return [cs.color, cs.backgroundColor];
    });
  const [netboxInk, netboxGround] = await colours(netbox);
  // The walk runs in the dark theme, so these are the dark inks (tokens.css).
  expect(netboxInk).toBe('rgb(90, 166, 255)');
  expect(netboxGround).not.toBe('rgb(255, 255, 255)');

  const meraki = row('from-meraki').locator('.ntree-badge');
  await expect(meraki).toHaveText('M');
  await expect(meraki).toHaveAttribute('title', 'Created by the Cisco Meraki integration');
  const [merakiInk, merakiGround] = await colours(meraki);
  expect(merakiInk).toBe('rgb(124, 195, 90)');
  expect(merakiGround).not.toBe('rgb(103, 179, 70)');

  // A touch screen cannot hover the letter, so opening the folder is where the word is readable.
  await row('from-netbox').click();
  await expect(page.locator('.nd-namewrap .nd-kind')).toHaveText('NetBox');

  // The row is there, and unmarked. Counting the badge on a row that was never found would pass
  // just as well, so the row is counted first.
  await expect(row('by-hand')).toHaveCount(1);
  await expect(row('by-hand').locator('.ntree-badge')).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
});
