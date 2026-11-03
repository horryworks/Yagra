// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Alert rules ▸ Events: a rule aimed at a folder (ADR-232). The dialog holds Save until a
// folder is chosen — sent empty, a folder rule would be a rule on every node — and the request
// carries the folder and no node.
import { expect, test } from '../support/app';

test('a folder rule waits for its folder and is sent with it alone', async ({ page, errors }) => {
  await page.goto('/alerts/rules/events');
  await page.getByRole('button', { name: '+ Add rule' }).click({ timeout: 15_000 });
  const dialog = page.getByRole('dialog');
  await dialog.locator('.modal-field', { hasText: 'Name' }).first().locator('input').fill('psu');
  await dialog.locator('#event-rule-pattern').fill('PSU');
  await dialog.locator('#event-rule-target').selectOption('folder');
  const save = dialog.getByRole('button', { name: 'Add rule' });
  await expect(save).toBeDisabled();

  await dialog.locator('.grouppick-trigger').click();
  const folder = page.locator('.grouppick-pop [role="option"]').nth(1);
  await folder.click();
  await expect(save).toBeEnabled();

  const post = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/event-rules',
  );
  await save.click();
  const body = (await post).postDataJSON() as { node_id: string | null; group_id: string | null };
  expect(body.node_id).toBeNull();
  expect(body.group_id).toMatch(/^[0-9a-f-]{36}$/);
  expect(errors.uncaught).toEqual([]);
});
