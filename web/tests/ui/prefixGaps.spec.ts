// SPDX-License-Identifier: AGPL-3.0-only
// A folder's pane lists the subnets its devices carry that its IP prefixes do not cover (ADR-170).
//
// Why Tier1: what each line says is `prefixGaps.ts`, unit-tested. What Vitest cannot run is the
// section in `PrefixGapsSection.tsx` that fetches the report and draws it — and the walk cannot
// stand in, because it never opens a folder's pane.
//
// The report is the generated body, patched — a hand-written one would be a second copy of the
// contract (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { CHILD_ID, membersByGroup, twoLevelGroups } from '../support/twoLevelTree';

function report(): Json {
  const template = defaultBodyFor('/api/v1/node-groups/{id}/prefix-gaps') as Record<string, Json>;
  const [gapTemplate] = (template.gaps as Record<string, Json>[]) ?? [{}];
  const seen = (n: number) => ({
    node_id: `00000000-0000-4000-8000-00000000009${n}`,
    ifindex: n,
    if_name: `Vlan${n}0`,
    ip: `192.0.2.${n}`,
  });
  return {
    ...template,
    group_id: CHILD_ID,
    nodes_total: 5,
    nodes_with_addresses: 4,
    nodes_truncated: 0,
    subnets_checked: 7,
    gaps: [
      {
        ...gapTemplate,
        subnet: '192.0.2.0/24',
        kind: 'unregistered',
        range: null,
        range_group: null,
        range_group_name: null,
        node_count: 3,
        seen_on: [seen(1)],
      },
      {
        ...gapTemplate,
        subnet: '198.51.100.0/24',
        kind: 'other_folder',
        range: '198.51.100.0/24',
        range_group: '00000000-0000-4000-8000-0000000000f1',
        range_group_name: 'site-b',
        node_count: 1,
        seen_on: [seen(2)],
      },
    ],
  } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': twoLevelGroups(),
      '/api/v1/nodes/by-group': membersByGroup,
      '/api/v1/node-groups/{id}/prefix-gaps': report(),
    },
  },
});

test("a folder's pane lists the subnets missing from its IP prefixes, with why", async ({
  page,
  errors,
}) => {
  await page.goto(`/nodes?sel=group:${CHILD_ID}`);
  const section = page
    .locator('section')
    .filter({ has: page.locator('.nd-section-t', { hasText: 'Subnets missing from the IP prefixes' }) });
  await expect(section).toHaveCount(1);

  await expect(section.locator('.nd-gap-read')).toHaveText('Addresses read from 4 of 5 devices.');
  const rows = section.locator('.nd-gap');
  await expect(rows).toHaveCount(2);

  await expect(rows.nth(0).locator('.nd-prefix-cidr')).toHaveText('192.0.2.0/24');
  await expect(rows.nth(0).locator('.nd-gap-kind')).toHaveText('Not in any IP prefix');
  await expect(rows.nth(0)).toContainText('Vlan10 192.0.2.1');
  await expect(rows.nth(0)).toContainText('+2 more devices');

  await expect(rows.nth(1).locator('.nd-prefix-cidr')).toHaveText('198.51.100.0/24');
  await expect(rows.nth(1).locator('.nd-gap-kind')).toHaveText(
    'In 198.51.100.0/24, registered to site-b',
  );

  expect(errors.uncaught).toEqual([]);
});
