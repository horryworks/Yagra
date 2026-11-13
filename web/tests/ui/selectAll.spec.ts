// SPDX-License-Identifier: AGPL-3.0-only
// The "select all" box over a table's rows (ADR-256).
//
// WHY A BROWSER. The three states live in a DOM property (`indeterminate`) that React cannot
// render, and the phone's row above the cards exists only in card mode — both in `.tsx` files
// Vitest never runs. What a press selects is `lib/selectAll.test.ts`'s.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

type Schemas = components['schemas'];

const proposal = (n: number) =>
  ({
    node_id: `00000000-0000-4000-8000-0000000001a${n}`,
    node_name: `ymock-node-${n}`,
    suggested_profile_id: '00000000-0000-4000-8000-0000000002a0',
    suggested_profile_name: 'ymock-profile',
    sys_object_id: '1.3.6.1.4.1.9.1.1',
  }) satisfies Schemas['ReclassifyProposal'];

const view = {
  proposals: [proposal(1), proposal(2), proposal(3)],
  total: 3,
  locked: 0,
  unidentified: 0,
} as unknown as Json;

test.use({ mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/reclassify': view } } });

test('the header box selects every row, shows a dash for some, and clears them all', async ({ page }) => {
  await page.goto('/nodes/collection/reclassify');
  const all = page.locator('.dt-head').getByRole('checkbox', { name: 'Select all' });
  const rows = page.locator('.dt-body').getByRole('checkbox');
  await expect(rows).toHaveCount(3);
  await expect(all).not.toBeChecked();

  await all.click();
  for (const box of await rows.all()) await expect(box).toBeChecked();
  await expect(all).toBeChecked();

  await page.getByRole('checkbox', { name: 'Select ymock-node-2' }).click();
  await expect(all).not.toBeChecked();
  expect(await all.evaluate((el) => (el as HTMLInputElement).indeterminate)).toBe(true);

  // From "some" a press selects everything; from "all" it clears everything.
  await all.click();
  await expect(all).toBeChecked();
  await all.click();
  for (const box of await rows.all()) await expect(box).not.toBeChecked();
  expect(await all.evaluate((el) => (el as HTMLInputElement).indeterminate)).toBe(false);
});

test('a phone gets one select-all row above the cards, not a box in every card', async ({ page }) => {
  // The fixture seeds `uiMode: 'desktop'`; this overwrites it (see `columnResize.spec.ts`).
  await page.setViewportSize({ width: 390, height: 780 });
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode: 'auto' }, version: 0 }),
    );
  });
  await page.goto('/nodes/collection/reclassify');
  await expect(page.locator('html')).toHaveAttribute('data-viewport', 'mobile');
  await expect(page.locator('.dt-card')).toHaveCount(3);
  await expect(page.locator('.dt-card').getByRole('checkbox', { name: 'Select all' })).toHaveCount(0);

  const bar = page.locator('.dt-selectall');
  await expect(bar).toContainText('0 / 3');
  await bar.getByRole('checkbox').click();
  await expect(bar).toContainText('3 / 3');
  for (const box of await page.locator('.dt-card').getByRole('checkbox').all()) {
    await expect(box).toBeChecked();
  }
});
