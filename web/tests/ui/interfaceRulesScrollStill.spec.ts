// SPDX-License-Identifier: AGPL-3.0-only
// The port's rule list stays where it was across a delete or an edit (ADR-203 item 11).
//
// The dialog used to swap its list for the confirm step (and for the form): returning from either
// mounted a fresh list at the top, so an operator who scrolled to the rule they wanted, opened it
// and cancelled had to find it again.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ae';
const IFINDEX = 1;
const RULES = 30;

function portRules(): Json {
  const [rule] = (defaultBodyFor('/api/v1/thresholds') as Record<string, Json>).items as Record<
    string,
    Json
  >[];
  return Array.from({ length: RULES }, (_, i) => ({
    rule: {
      ...rule,
      id: `00000000-0000-4000-8000-00000000e${String(i).padStart(3, '0')}`,
      scope_level: 'interface',
      scope_ids: [`${NODE_ID}:${IFINDEX}`],
      metric: 'if_in_util_pct',
      direction: 'above',
      warning: null,
      critical: 50 + i,
      dwell_samples: 3,
    },
    in_force: i === 0,
  })) as unknown as Json;
}

test.use({
  viewport: { width: 1280, height: 800 },
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
  expect(room, 'the list fits the dialog — this fixture cannot see the bug').toBeGreaterThan(300);
  const box = (await body.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 80);
  await page.mouse.wheel(0, 400);
  await expect.poll(() => body.evaluate((el) => el.scrollTop)).toBeGreaterThan(200);
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
      return b.top > box.top + 20 && b.bottom < box.bottom - 20;
    });
  });
  expect(i).toBeGreaterThan(2);
  return body.locator('.ifrules-row').nth(i);
}

test('cancelling a delete returns to the list where it was', async ({ page }) => {
  const { body, at } = await scrolledRules(page);
  await (await visibleRow(body)).getByRole('button', { name: 'Delete' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: /delete/i }).last();
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
