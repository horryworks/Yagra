// SPDX-License-Identifier: AGPL-3.0-only
// The node Overview's availability graph (ADR-223). `availability.ts` is unit-tested; what Vitest
// cannot reach is the wiring in `AvailabilitySection.tsx`: that a period draws its number of bars,
// that the outage reaches the screen as red, and that picking a bar — by press or by keyboard —
// changes the line under the strip. The mock answer is the one `bootstrap.ts` serves every walk.

import { expect, test } from '../support/app';

const NODE_ID = '00000000-0000-4000-8000-000000000001';

test('the graph draws one bar per slice of the period and says which slice was down', async ({
  page,
  errors,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=overview`);
  const strip = page.getByRole('slider', { name: 'Availability per period' });
  await expect(strip).toBeVisible({ timeout: 15_000 });
  await expect(strip.locator('.nd-av-bar')).toHaveCount(30);
  await expect(page.locator('.nd-av-pct')).toHaveText('99.50%');
  await expect(page.locator('.nd-av-line')).toHaveText('1 outage · 1h 5m down');
  // One bar carries the outage (today's, or yesterday's just after midnight).
  const downBar = strip.locator('.nd-av-bar:has(.nd-av-down)');
  await expect(downBar).toHaveCount(1);

  // Pressing it says what it was; the keyboard walks to the oldest, which was clean.
  await downBar.click();
  await expect(page.locator('.nd-av-line')).toContainText('Down 1h 5m (1 outage)');
  await page.keyboard.press('Home');
  await expect(page.locator('.nd-av-line')).toContainText('No outage');

  await page.getByRole('button', { name: '24h' }).click();
  await expect(strip.locator('.nd-av-bar')).toHaveCount(48);
  expect(errors.uncaught).toEqual([]);
});
