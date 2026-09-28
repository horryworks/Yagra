// SPDX-License-Identifier: AGPL-3.0-only
// What a dialog does with its save — refused, partly applied, or dismissed (ADR-184).
//
// Written before the dialogs moved onto the shared submit (ADR-184 increment 34), against the code
// as it was, so the move is checked against what shipped rather than against what it became. The
// one assertion the move is meant to change is marked, and changes in the increment that changes it.
//
// Why a browser: the footer, the error line and the focus are all in `.tsx` files, which Vitest
// does not load (`web-vitest-node`), and "the dialog stayed open" is a statement about the DOM.

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

async function openAddUser(page: Page) {
  await page.goto('/settings/users');
  const open = page.getByRole('button', { name: 'Add user' }).first();
  await open.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await dialog.getByRole('textbox').first().fill('ops-01');
  await dialog.locator('input[type="password"]').first().fill('correct-horse');
  return { open, dialog };
}

test.describe('a refused save', () => {
  test.use({
    mockConfig: {
      overrides: BOOTSTRAP_OVERRIDES,
      failures: { '/api/v1/users': { status: 500, code: 'internal', method: 'POST' } },
    },
  });

  test('keeps the dialog open, says why, and leaves both buttons usable', async ({
    page,
    mock,
  }) => {
    const { dialog } = await openAddUser(page);
    await dialog.getByRole('button', { name: 'Add user' }).click();

    await expect.poll(() => mock.served).toContain('POST /api/v1/users');
    await expect(dialog.locator('.form-error')).toContainText('forced 500');
    await expect(dialog).toBeVisible();
    // Busy is over: the operator can try again, or give up.
    await expect(dialog.getByRole('button', { name: 'Add user' })).toBeEnabled();
    const cancel = dialog.getByRole('button', { name: 'Cancel', exact: true });
    await expect(cancel).toBeEnabled();
    await cancel.click();
    await expect(dialog).toBeHidden();
  });
});

test('an accepted save closes the dialog', async ({ page, mock }) => {
  const { dialog } = await openAddUser(page);
  await dialog.getByRole('button', { name: 'Add user' }).click();
  await expect.poll(() => mock.served).toContain('POST /api/v1/users');
  await expect(dialog).toBeHidden();
});

/** Three nodes in the inventory tree's working set. */
async function checkThree(page: Page) {
  await page.goto('/nodes');
  const rows = page.locator('.ntree-node');
  await expect(rows).toHaveCount(3);
  await rows.nth(0).click();
  await rows.nth(2).click({ modifiers: ['Shift'] });
  await expect(page.locator('.ntree-row.checked')).toHaveCount(3);
}

/** A batch the server applied to one of the three it was asked for. */
const ONE_OF_THREE = { requested: 3, applied: 1, moved: 1 };

test.describe('a batch the server only partly applied', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/pool': ONE_OF_THREE,
        '/api/v1/nodes/tags': ONE_OF_THREE,
        '/api/v1/nodes/move': ONE_OF_THREE,
      },
    },
  });

  test('pool: stays open and names both numbers', async ({ page }) => {
    await checkThree(page);
    await page.getByRole('button', { name: 'More…' }).click();
    await page.getByRole('menuitem', { name: 'Poller pool…' }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('textbox').fill('site-a');
    await dialog.getByRole('button', { name: 'Save' }).click();

    await expect(dialog.locator('.form-error')).toContainText('1 of 3');
    await expect(dialog).toBeVisible();
    // ⚠️ Today's wording. Increment 39 turns this into "Close": a write happened, and "Cancel"
    // reads as undoing it.
    await expect(
      dialog.locator('.modal-footer').getByRole('button', { name: 'Cancel', exact: true }),
    ).toBeEnabled();
  });

  test('tags: stays open and names both numbers', async ({ page }) => {
    await checkThree(page);
    await page.getByRole('button', { name: 'Tag…' }).click();
    const dialog = page.getByRole('dialog');
    const add = dialog.getByRole('textbox').first();
    await add.fill('core');
    await add.press('Enter');
    await dialog.getByRole('button', { name: 'Apply' }).click();

    await expect(dialog.locator('.form-error')).toContainText('1 of 3');
    await expect(dialog).toBeVisible();
    // ⚠️ Today's wording — see the pool test.
    await expect(
      dialog.locator('.modal-footer').getByRole('button', { name: 'Cancel', exact: true }),
    ).toBeEnabled();
  });

  test('move: stays open, names both numbers, and offers Close', async ({ page }) => {
    await checkThree(page);
    // The tree's menu, not the selection bar: the detail pane has a "Move…" of its own.
    await page.locator('.ntree-node').nth(1).click({ button: 'right' });
    await page.getByRole('menu').getByRole('button', { name: 'Move 3 selected…' }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('button', { name: 'Move', exact: true }).click();

    await expect(dialog.locator('.form-error')).toContainText('1 of 3');
    await expect(dialog).toBeVisible();
    await expect(
      dialog.locator('.modal-footer').getByRole('button', { name: 'Close', exact: true }),
    ).toBeEnabled();
  });
});
