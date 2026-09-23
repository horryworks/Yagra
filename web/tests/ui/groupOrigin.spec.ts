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

  // The badge's own text is the fact; the title only says it in a sentence.
  const netbox = row('from-netbox').locator('.ntree-badge');
  await expect(netbox).toHaveText('NetBox');
  await expect(netbox).toHaveAttribute('title', 'Created by the NetBox integration');
  // NetBox's blue on white (2026-09-23), Meraki's white on green: each wears its own colours.
  const colours = (badge: typeof netbox) =>
    badge.evaluate((el) => {
      const cs = getComputedStyle(el);
      return [cs.color, cs.backgroundColor];
    });
  expect(await colours(netbox)).toEqual(['rgb(22, 133, 252)', 'rgb(255, 255, 255)']);

  const meraki = row('from-meraki').locator('.ntree-badge');
  await expect(meraki).toHaveText('Meraki');
  await expect(meraki).toHaveAttribute('title', 'Created by the Cisco Meraki integration');
  expect(await colours(meraki)).toEqual(['rgb(255, 255, 255)', 'rgb(103, 179, 70)']);

  // The row is there, and unmarked. Counting the badge on a row that was never found would pass
  // just as well, so the row is counted first.
  await expect(row('by-hand')).toHaveCount(1);
  await expect(row('by-hand').locator('.ntree-badge')).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
});
