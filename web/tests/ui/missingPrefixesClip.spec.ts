// SPDX-License-Identifier: AGPL-3.0-only
// Nodes > Missing IP prefixes: an opened site's long text is cut with an ellipsis and readable in
// full, and nothing runs into the column beside it (ADR-088 Inc.4).
//
// Why its own spec: the walk never opens a row, so the site detail is outside its geometry check,
// and the mock's strings are too short to be cut anyway. This one serves long ones, opens the
// site, and runs the same check over the opened detail.
//
// The report is the generated body, patched — a hand-written one would be a second copy of the
// contract (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { inspectScreenGeometry } from './screenGeometry';

const LONG_FOLDER = 'site-a building north wing second floor distribution closet';
const LONG_OWNER = 'site-b building south wing third floor distribution closet';

function report(): Json {
  const template = defaultBodyFor('/api/v1/prefix-gaps') as Record<string, Json>;
  const [siteTemplate] = (template.sites as Record<string, Json>[]) ?? [{}];
  const [gapTemplate] = ((siteTemplate.gaps as Record<string, Json>[]) ?? [{}]);
  const gaps = [
    {
      ...gapTemplate,
      subnet: '198.51.100.0/24',
      kind: 'other_folder',
      range: '198.51.0.0/16',
      range_group: '00000000-0000-4000-8000-0000000000f1',
      range_group_name: LONG_OWNER,
      node_count: 1,
      intentional: null,
      seen_on: [
        {
          node_id: '00000000-0000-4000-8000-000000000091',
          ifindex: 1,
          if_name: 'Vlan1200',
          ip: '198.51.100.90',
        },
      ],
    },
  ];
  return {
    ...template,
    gaps_total: 1,
    gaps_listed: 1,
    sites: [
      {
        ...siteTemplate,
        site_id: '00000000-0000-4000-8000-0000000000e1',
        name: LONG_FOLDER,
        path: ['region', 'country'],
        is_site: true,
        status: 'gaps',
        gap_count: 1,
        intentional_count: 0,
        nodes_total: 1,
        nodes_with_addresses: 1,
        nodes_truncated: 0,
        prefixes: 5,
        gaps,
      },
    ],
  } as unknown as Json;
}

test.use({
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/prefix-gaps': report() } },
  viewport: { width: 1000, height: 800 },
});

test("an opened site's long reason is cut with an ellipsis, and its links stay in their column", async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/missing-prefixes');
  await page.locator('.dt-row', { hasText: LONG_FOLDER }).first().click();
  const row = page.locator('.mp-gaps .ytable-row');
  await expect(row).toHaveCount(1);

  // The reason is long enough to be cut at this width, so the test is about a cut, not a fit.
  const reason = row.locator('.mp-kind .clipped-text');
  const full = await reason.getAttribute('title');
  expect(full).toContain(LONG_OWNER);
  const cut = await reason.evaluate((el) => ({
    over: el.scrollWidth - el.clientWidth,
    display: getComputedStyle(el).display,
    overflow: getComputedStyle(el).textOverflow,
  }));
  expect(cut.over).toBeGreaterThan(0);
  // A block box: the only kind on which `text-overflow` draws the ellipsis.
  expect(cut.display).toBe('block');
  expect(cut.overflow).toBe('ellipsis');

  // The folder link stays left of the mark button rather than running under it.
  const link = row.locator('.mp-links a').first();
  const button = row.getByRole('button', { name: 'Mark as intentional' });
  const linkBox = await link.locator('xpath=..').boundingBox();
  const buttonBox = await button.boundingBox();
  expect(linkBox && buttonBox).toBeTruthy();
  expect(linkBox!.x + linkBox!.width).toBeLessThanOrEqual(buttonBox!.x);
  expect(await link.locator('xpath=..').getAttribute('title')).toContain(LONG_FOLDER);

  // And the walk's own geometry check passes over the opened detail.
  const geometry = await inspectScreenGeometry(page);
  expect(geometry.findings).toEqual([]);
  expect(errors.uncaught).toEqual([]);
});
