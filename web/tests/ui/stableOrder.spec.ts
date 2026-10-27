// SPDX-License-Identifier: AGPL-3.0-only
// Acting on a row does not move it (ADR-216). Nodes > Missing IP prefixes lists the sites with
// the most open gaps first, so marking a gap lowers the site's count and the server's next answer
// puts it further down. The screen keeps the order it is showing until the operator changes what
// the list shows.
//
// Why its own spec: the walk never writes, and the order only moves between two answers. The
// answers are the generated body, patched (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Site = { name: string; id: string; open: number };

function answer(sites: Site[]): Json {
  const template = defaultBodyFor('/api/v1/prefix-gaps') as Record<string, Json>;
  const [siteTemplate] = (template.sites as Record<string, Json>[]) ?? [{}];
  const [gapTemplate] = (siteTemplate.gaps as Record<string, Json>[]) ?? [{}];
  const gap = (i: number, n: number) => ({
    ...gapTemplate,
    subnet: `198.51.${i * 10 + n}.0/24`,
    kind: 'no_range',
    range: null,
    range_group: null,
    range_group_name: null,
    node_count: 1,
    intentional: null,
    seen_on: [],
  });
  const total = sites.reduce((n, s) => n + s.open, 0);
  return {
    ...template,
    gaps_total: total,
    gaps_listed: total,
    sites: sites.map((s, i) => ({
      ...siteTemplate,
      site_id: s.id,
      name: s.name,
      path: [],
      is_site: true,
      status: 'gaps',
      gap_count: s.open,
      intentional_count: 0,
      nodes_total: 1,
      nodes_with_addresses: 1,
      nodes_truncated: 0,
      prefixes: 1,
      gaps: Array.from({ length: s.open }, (_, n) => gap(i, n)),
    })),
  } as unknown as Json;
}

const A = { name: 'site-a', id: '00000000-0000-4000-8000-0000000000a1' };
const B = { name: 'site-b', id: '00000000-0000-4000-8000-0000000000a2' };
const C = { name: 'site-c', id: '00000000-0000-4000-8000-0000000000a3' };

// Before the mark: a and b have two open gaps, c one — the server's order is a, b, c. After a
// gap of a is marked, a has one: the server's order is b, a, c.
const BEFORE = answer([
  { ...A, open: 2 },
  { ...B, open: 2 },
  { ...C, open: 1 },
]);
const AFTER = answer([
  { ...B, open: 2 },
  { ...A, open: 1 },
  { ...C, open: 1 },
]);

test.use({
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/prefix-gaps': BEFORE } },
});

const siteNames = (page: import('@playwright/test').Page) =>
  page.locator('.dt-row .mp-site-name .clipped-text').allTextContents();

test('marking a gap keeps the site where it was, until the tab changes', async ({ page, errors }) => {
  let marked = false;
  await page.route('**/api/v1/prefix-gaps', (route) =>
    route.request().method() === 'GET'
      ? route.fulfill({ json: marked ? AFTER : BEFORE })
      : route.fallback(),
  );
  await page.route('**/api/v1/prefix-gaps/acks', (route) => {
    marked = true;
    return route.fulfill({ status: 204, body: '' });
  });

  await page.goto('/nodes/missing-prefixes');
  await expect.poll(() => siteNames(page)).toEqual(['site-a', 'site-b', 'site-c']);

  await page.locator('.dt-row', { hasText: 'site-a' }).first().click();
  await page.locator('.mp-gaps .ytable-row').first().getByRole('button', { name: 'Mark as intentional' }).click();
  const reread = page.waitForResponse(
    (r) => r.request().method() === 'GET' && new URL(r.url()).pathname === '/api/v1/prefix-gaps',
  );
  await page.getByRole('dialog').getByRole('button', { name: 'Mark as intentional' }).click();
  await reread;

  // The new answer is drawn — a has one gap left on screen — in the order the operator was reading.
  await expect(page.locator('.mp-gaps .ytable-row')).toHaveCount(1);
  expect(await siteNames(page)).toEqual(['site-a', 'site-b', 'site-c']);

  // Changing the tab asks for a new order, and it is the server's.
  await page.getByRole('tab', { name: /Intentional/ }).click();
  await page.getByRole('tab', { name: /Gaps/ }).click();
  await expect.poll(() => siteNames(page)).toEqual(['site-b', 'site-a', 'site-c']);
  expect(errors.uncaught).toEqual([]);
});
