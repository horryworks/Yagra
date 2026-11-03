// SPDX-License-Identifier: AGPL-3.0-only
// The port's rule list stays where it was across a revert or an edit (ADR-203 item 11).
//
// The dialog used to swap its list for the confirm step (and for the form): returning from either
// mounted a fresh list at the top, so an operator who scrolled to the rule they wanted, opened it
// and cancelled had to find it again.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ae';
const IFINDEX = 1;
/** One rule of the port's own on every subject, so every row has an Edit and a Revert. */
const OWN: [string, string, number | null, number][] = [
  ['if_in_util_pct', 'above', 70, 90],
  ['if_out_util_pct', 'above', 70, 90],
  ['if_oper_status', 'above', null, 1.5],
  ['if_rx_power_dbm', 'below', -20, -25],
  ['if_tx_power_dbm', 'below', -6, -8],
];

function portRules(): Json {
  const [rule] = (defaultBodyFor('/api/v1/thresholds') as Record<string, Json>).items as Record<
    string,
    Json
  >[];
  return OWN.map(([metric, direction, warning, critical], i) => ({
    rule: {
      ...rule,
      id: `00000000-0000-4000-8000-00000000e${String(i).padStart(3, '0')}`,
      scope_level: 'interface',
      scope_ids: [`${NODE_ID}:${IFINDEX}`],
      metric,
      direction,
      warning,
      critical,
      dwell_samples: 3,
    },
    in_force: true,
  })) as unknown as Json;
}

test.use({
  // Short, so five rows overflow the dialog and there is a scroll position to lose.
  viewport: { width: 1280, height: 420 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces/{ifindex}/thresholds': () => portRules(),
    },
  },
});

type Page = import('@playwright/test').Page;

async function scrolledRules(page: Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await page.locator('.nd-if-row .nd-if-name').first().click({ timeout: 15_000 });
  await page.locator('.nd-if-dock-rule').click({ timeout: 15_000 });
  const body = page.getByRole('dialog').locator('.modal-body');
  await expect(body.locator('.ifrules-row').first()).toBeVisible();
  const room = await body.evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(room, 'the list fits the dialog — this fixture cannot see the bug').toBeGreaterThan(40);
  const box = (await body.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 40);
  await page.mouse.wheel(0, 400);
  await expect.poll(() => body.evaluate((el) => el.scrollTop)).toBeGreaterThan(20);
  await page.waitForTimeout(100);
  return { body, at: await body.evaluate((el) => el.scrollTop) };
}

/** A rule row fully inside the scrolled list, so pressing it needs no scroll of its own. */
async function visibleRow(body: import('@playwright/test').Locator) {
  const i = await body.evaluate((el) => {
    const box = el.getBoundingClientRect();
    const rows = [...el.querySelectorAll<HTMLElement>('.ifrules-row')];
    return rows.findIndex((r) => {
      const b = r.getBoundingClientRect();
      return b.top >= box.top && b.bottom <= box.bottom;
    });
  });
  expect(i).toBeGreaterThan(0);
  return body.locator('.ifrules-row').nth(i);
}

test('cancelling a revert returns to the list where it was', async ({ page }) => {
  const { body, at } = await scrolledRules(page);
  await (await visibleRow(body)).getByRole('button', { name: 'Revert' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: /inherit/i }).last();
  await confirm.getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(1);
  expect(await body.evaluate((el) => el.scrollTop), 'the list came back at the top').toBe(at);
});

test('cancelling an edit returns to the list where it was', async ({ page }) => {
  const { body, at } = await scrolledRules(page);
  await (await visibleRow(body)).getByRole('button', { name: 'Edit' }).click();
  await page.getByRole('dialog').last().getByRole('button', { name: 'Cancel', exact: true }).click();
  await expect(page.getByRole('dialog')).toHaveCount(1);
  expect(await body.evaluate((el) => el.scrollTop), 'the list came back at the top').toBe(at);
});
