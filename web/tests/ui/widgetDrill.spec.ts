// SPDX-License-Identifier: AGPL-3.0-only
// A widget's parts open the screen narrowed to what was pressed (ADR-250).
//
// `drill.test.ts` pins the hrefs and the route ledger pins that the destinations read their keys.
// What neither can see is that the parts are drawn as links at all, that pressing one lands where
// the href says, and that the public board draws none — `.tsx` never runs under Vitest.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

const BOARD = {
  version: 3,
  boards: [
    {
      id: 'b1',
      name: 'Board',
      widgets: [
        { instanceId: 'w1', type: 'top-cpu', span: 6 },
        { instanceId: 'w2', type: 'nodes-down', span: 6 },
        { instanceId: 'w3', type: 'busiest-interfaces', span: 6 },
        { instanceId: 'w4', type: 'active-alerts', span: 6 },
      ],
    },
  ],
};

const TOP = { entries: [{ node_id: NODE_ID, name: 'core-sw', value: 42 }], partial: false } as unknown as Json;

const IF_TOP = {
  entries: [
    { node_id: NODE_ID, node_name: 'core-sw', ifindex: 3, if_name: 'Gi0/3', if_alias: null, value: 1e6 },
  ],
  partial: false,
} as unknown as Json;

/** Two ports on the node, the second being the one the dashboard row names. */
function interfaceRows(): Json {
  const [row] = defaultBodyFor(`/api/v1/nodes/${NODE_ID}/interfaces`) as Record<string, unknown>[];
  const port = (ifindex: number, if_name: string) => ({ ...row, ifindex, if_name, oper_status: 1, stale: false });
  return [port(1, 'Gi0/1'), port(3, 'Gi0/3')] as unknown as Json;
}

test.use({
  viewport: { width: 1440, height: 900 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/dashboard': () => BOARD,
      '/api/v1/public-dashboard': () => BOARD,
      '/api/v1/metrics/top': () => TOP,
      '/api/v1/metrics/interface-top': () => IF_TOP,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces': () => interfaceRows(),
    },
  },
});

function cell(page: import('@playwright/test').Page, title: string) {
  return page.locator(`.mydash-cell:has(.widgetframe-kind:text-is("${title}"))`);
}

test('a ranked row opens its node', async ({ page, errors }) => {
  await page.goto('/dashboard/my');
  const row = cell(page, 'Top CPU').locator('a.rankedbar');
  await expect(row).toHaveCount(1, { timeout: 15_000 });
  await row.click();
  await expect(page).toHaveURL(new RegExp(`/nodes/${NODE_ID}$`));
  expect(errors.uncaught).toEqual([]);
});

test('the nodes-down figure opens the down nodes', async ({ page }) => {
  await page.goto('/dashboard/my');
  await cell(page, 'Nodes down').locator('a.kpitile').click();
  await expect(page).toHaveURL(/\/nodes\?/);
  expect(new URL(page.url()).searchParams.get('state')).toBe('critical,unreachable');
});

test('a card title opens the list it summarizes, outside Customize only', async ({ page }) => {
  await page.goto('/dashboard/my');
  const title = cell(page, 'Active alerts').locator('a.widgetframe-kind');
  await expect(title).toHaveCount(1, { timeout: 15_000 });
  await page.getByRole('button', { name: 'Customize' }).click();
  await expect(cell(page, 'Active alerts').locator('a.widgetframe-kind')).toHaveCount(0);
  await page.getByRole('button', { name: 'Done' }).click();
  await cell(page, 'Active alerts').locator('a.widgetframe-kind').click();
  await expect(page).toHaveURL(/\/alerts$/);
});

test('an interface row opens its node with that port already open', async ({ page }) => {
  await page.goto('/dashboard/my');
  await cell(page, 'Busiest interfaces').locator('a.rankedbar').click();
  await expect(page).toHaveURL(new RegExp(`/nodes/${NODE_ID}\\?`));
  expect(new URL(page.url()).searchParams.get('tab')).toBe('interfaces');
  // That port's row is the selected one, which is what opens the dock under the list.
  await expect(page.locator('.nd-if-row.selected')).toContainText('Gi0/3', { timeout: 15_000 });
  // The request is applied and then leaves the URL, so the next device's port 3 is not opened.
  await expect.poll(() => new URL(page.url()).searchParams.get('interfaces.port')).toBeNull();
});

test('the public board draws no links', async ({ page }) => {
  await page.goto('/dashboard/public');
  // The rows are there, so an absence of links is about links and not about an empty board.
  await expect(cell(page, 'Top CPU').locator('.rankedbar')).toHaveCount(1, { timeout: 15_000 });
  await expect(page.locator('.mydash-cell a')).toHaveCount(0);
});
