// SPDX-License-Identifier: AGPL-3.0-only
// Discovery's Scan a range card (ADR-244): the IP range | Site toggle, the target and Scan on one
// row, and the folded settings.
//
// WHY A BROWSER. The layout is the whole change. The first build drew the toggle across the full
// width of the card because the shared segmented control stretches its buttons, and nothing but a
// laid-out page can see that.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

/** One folder that carries an IP range, so the toggle is drawn. */
const groups = (() => {
  const [first] = defaultBodyFor('/api/v1/node-groups') as unknown as Record<string, unknown>[];
  return [
    {
      ...first,
      id: '00000000-0000-4000-8000-0000000000c1',
      parent_id: null,
      origin: null,
      name: 'ymock-site',
      prefixes: ['192.0.2.0/24'],
    },
  ] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': groups,
      '/api/v1/discovery/scans': [],
    },
  },
});

test('the toggle is as wide as its two words, and Scan sits beside the target', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/discovery');
  const toggle = page.locator('.disco-toggle .ts-seg');
  await expect(toggle.getByRole('button', { name: 'IP range' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  const width = (await toggle.boundingBox())?.width ?? 0;
  expect(width).toBeGreaterThan(80);
  expect(width).toBeLessThan(260);

  const field = await page.locator('textarea.disco-range').boundingBox();
  const scan = await page.getByRole('button', { name: 'Scan', exact: true }).boundingBox();
  expect(field && scan).toBeTruthy();
  // Same row: the button's top is within the field's height of the field's top.
  expect(Math.abs((scan?.y ?? 0) - (field?.y ?? 0))).toBeLessThan(field?.height ?? 0);

  // Site swaps the range field for the site picker and its ranges, and keeps the toggle's width.
  await toggle.getByRole('button', { name: 'Site' }).click();
  await expect(page.locator('textarea.disco-range')).toHaveCount(0);
  await expect(page.locator('.disco-prefix-cidr')).toHaveText(['192.0.2.0/24']);
  expect((await toggle.boundingBox())?.width ?? 0).toBeLessThan(260);
  expect(errors.uncaught).toEqual([]);
});
