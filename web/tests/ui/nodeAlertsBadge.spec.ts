// SPDX-License-Identifier: AGPL-3.0-only
// The state badge beside a node's name opens that node's alerts on any tab, and each row takes the
// operator to where it is read (ADR-238).
//
// WHY A BROWSER. Which tab a row jumps to is unit-tested (`alertJump.test.ts`). What is not: that
// the badge is a button only while alerts exist, that the list opens over a tab other than
// Overview, and that a port alert lands on the Interfaces tab with that port selected and its dock
// open. The last one crosses two components and an unmounted tab, and all of it is `.tsx`.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const NODE_ID = '00000000-0000-4000-8000-0000000000ad';
const QUIET_ID = '00000000-0000-4000-8000-0000000000ae';

function interfaceRows(): Json {
  const [row] = defaultBodyFor(`/api/v1/nodes/${NODE_ID}/interfaces`) as Record<string, unknown>[];
  const port = (ifindex: number, name: string) => ({
    ...row,
    ifindex,
    if_name: name,
    if_alias: null,
    oper_status: 1,
    stale: false,
    addresses: [],
  });
  return [port(10, 'Gi1/0/10'), port(11, 'Gi1/0/11'), port(12, 'Gi1/0/12')] as unknown as Json;
}

function status(nodeId: string, withAlerts: boolean): Json {
  const body = defaultBodyFor('/api/v1/nodes/{node_id}/status') as unknown as Schemas['NodeStatus'];
  const [first] = body.alerts;
  const alert = (o: Record<string, unknown>) => ({
    ...first,
    node: nodeId,
    severity: 'warning',
    state: 'warning',
    flapping: false,
    root_cause: null,
    ifindex: null,
    if_name: null,
    row: null,
    row_name: null,
    ...o,
  });
  return {
    ...body,
    node_id: nodeId,
    state: withAlerts ? 'warning' : 'ok',
    collection_fault: null,
    alerts: withAlerts
      ? [
          alert({ check: `${nodeId}:ifInErrors:12`, metric: 'ifInErrors', ifindex: 12, if_name: 'Gi1/0/12' }),
          alert({ check: `${nodeId}:cpu`, metric: 'cpu_5min' }),
        ]
      : [],
  } as unknown as Json;
}

test.use({
  viewport: { width: 1400, height: 900 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': (url: URL) =>
        deviceNode(url.pathname.includes(QUIET_ID) ? QUIET_ID : NODE_ID),
      '/api/v1/nodes/{node_id}/status': (url: URL) =>
        url.pathname.includes(QUIET_ID) ? status(QUIET_ID, false) : status(NODE_ID, true),
      '/api/v1/nodes/{node_id}/interfaces': () => interfaceRows(),
    },
  },
});

const badge = (page: import('@playwright/test').Page) =>
  page.locator('.nd-namewrap').getByRole('button', { name: /Warning/ });

test('the badge opens the alerts on another tab, and a port alert opens that port', async ({
  page,
  errors,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=events`);
  await expect(badge(page)).toBeVisible({ timeout: 15_000 });
  await expect(badge(page).locator('.nd-statepill-n')).toHaveText('2');
  // The state word stands alone, so a reader comparing it with the list is not handed the count.
  await expect(badge(page).locator('.nd-statepill-label')).toHaveText('Warning');

  await badge(page).click();
  const list = page.getByRole('dialog', { name: 'Active alerts' });
  await expect(list.locator('.nd-alert')).toHaveCount(2);
  // Away from Overview both rows offer a jump: the port to Interfaces, the CPU one to Overview.
  await expect(list.getByRole('button', { name: /Interfaces/ })).toHaveCount(1);
  await expect(list.getByRole('button', { name: /Overview/ })).toHaveCount(1);

  await list.getByRole('button', { name: /Interfaces/ }).click();
  await expect(list).toBeHidden();
  await expect(page).toHaveURL(/tab=interfaces/);
  const selected = page.locator('.nd-if-row.selected');
  await expect(selected).toHaveCount(1, { timeout: 15_000 });
  await expect(selected).toContainText('Gi1/0/12');

  // Escape closes the list and gives focus back to the badge.
  await badge(page).click();
  await expect(list).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(list).toBeHidden();
  await expect(badge(page)).toBeFocused();
  expect(errors.uncaught).toEqual([]);
});

test('on Overview the list does not offer Overview', async ({ page }) => {
  await page.goto(`/nodes/${NODE_ID}?tab=overview`);
  await badge(page).click();
  const list = page.getByRole('dialog', { name: 'Active alerts' });
  await expect(list.locator('.nd-alert')).toHaveCount(2);
  await expect(list.getByRole('button', { name: /Overview/ })).toHaveCount(0);
  // The section on the tab itself is still there.
  await expect(page.locator('.nd-body .nd-alert')).toHaveCount(2);
});

test('a node with no alerts keeps the plain badge', async ({ page }) => {
  await page.goto(`/nodes/${QUIET_ID}?tab=overview`);
  const pill = page.locator('.nd-namewrap .nd-statepill');
  await expect(pill).toBeVisible({ timeout: 15_000 });
  await expect(pill).toHaveText(/OK/i);
  await expect(page.locator('.nd-namewrap button.nd-statepill')).toHaveCount(0);
});
