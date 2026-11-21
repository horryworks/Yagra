// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Maintenance windows: a window is edited after it was made (ADR-261 Inc.5). The row's
// menu opens the add dialog filled from the window, on the clock it was asked in, with its end as
// a picked end; Save replaces the window through its own route. The upgrade's fleet-wide window
// offers no Edit.
import { expect, test } from '../support/app';

const ID = '00000000-0000-4000-8000-0000000000aa';
const NODE = '00000000-0000-4000-8000-000000000001';

const windows = [
  {
    id: ID,
    name: 'Core swap',
    scope_level: 'node',
    scope_id: NODE,
    starts_at: '2030-01-02T13:00:00+00:00',
    ends_at: '2030-01-02T17:30:00+00:00',
    enabled: true,
    active: false,
    time_zone: 'Asia/Tokyo',
  },
  {
    id: '00000000-0000-4000-8000-0000000000bb',
    name: 'Upgrade',
    scope_level: 'system',
    scope_id: '',
    starts_at: '2030-01-01T00:00:00+00:00',
    ends_at: '2030-01-01T00:15:00+00:00',
    enabled: true,
    active: false,
  },
];

test('a window opens filled in and saves through its own route', async ({ page, errors }) => {
  await page.route('**/api/v1/maintenance-windows', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await route.fulfill({ json: windows });
  });
  await page.goto('/alerts/maintenance');

  // On a desktop the row's actions are icon buttons named after each action.
  const row = page.locator('.dt-row', { hasText: 'Core swap' });
  await row.getByRole('button', { name: 'Edit' }).click({ timeout: 15_000 });

  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('Edit maintenance window')).toBeVisible();
  await expect(dialog.getByPlaceholder('Firmware upgrade, ticket CHG-1234')).toHaveValue(
    'Core swap',
  );
  await expect(dialog.locator('#maint-start-date')).toHaveValue('2030-01-02');
  await expect(dialog.getByLabel('Start time')).toHaveValue('22:00');
  await expect(dialog.locator('#maint-end-date')).toHaveValue('2030-01-03');
  await expect(dialog.getByLabel('End time')).toHaveValue('02:30');

  await dialog.getByPlaceholder('Firmware upgrade, ticket CHG-1234').fill('Core swap, CHG-9');
  const put = page.waitForRequest(
    (r) =>
      r.method() === 'PUT' &&
      new URL(r.url()).pathname === `/api/v1/maintenance-windows/${ID}/definition`,
  );
  await dialog.getByRole('button', { name: 'Save' }).click();
  const body = (await put).postDataJSON() as Record<string, string>;
  expect(body).toMatchObject({
    name: 'Core swap, CHG-9',
    scope_level: 'node',
    scope_id: NODE,
    starts_at: '2030-01-02T13:00:00.000Z',
    ends_at: '2030-01-02T17:30:00.000Z',
    time_zone: 'Asia/Tokyo',
  });

  // The upgrade's window is the upgrade's to end: its menu has no Edit.
  await expect(dialog).toBeHidden();
  const upgrade = page.locator('.dt-row', { hasText: 'Upgrade' });
  await expect(upgrade.getByRole('button', { name: 'Disable' })).toHaveCount(1);
  await expect(upgrade.getByRole('button', { name: 'Edit' })).toHaveCount(0);
  expect(errors.uncaught).toEqual([]);
});
