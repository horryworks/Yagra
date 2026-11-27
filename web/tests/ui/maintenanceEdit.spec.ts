// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Maintenance windows: a window is edited after it was made (ADR-261 Inc.5). The row's
// menu opens the add dialog filled from the window, on the clock it was asked in; Save replaces
// the window through its own route. The upgrade's fleet-wide window offers no Edit. The second
// test drives the time boxes' ▲▼ and the end that follows the start (Inc.6); the third opens a new
// window filled in and keeps its end after its start (Inc.7).
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
  await expect(dialog.getByLabel('Start time, hour', { exact: true })).toHaveValue('22');
  await expect(dialog.getByLabel('Start time, minute', { exact: true })).toHaveValue('00');
  await expect(dialog.locator('#maint-end-date')).toHaveValue('2030-01-03');
  await expect(dialog.getByLabel('End time, hour', { exact: true })).toHaveValue('02');
  await expect(dialog.getByLabel('End time, minute', { exact: true })).toHaveValue('30');

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

test('the end follows the start, every box steps, and the two rows line up', async ({
  page,
  errors,
}) => {
  // ADR-261 Inc.6: the end is always a date and a time; ▲▼ step a box (an hour past midnight
  // carries the date); moving the start moves the end by the same amount; a shortcut fills the
  // end from the start; and the start and end rows put their boxes in the same columns.
  await page.route('**/api/v1/maintenance-windows', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await route.fulfill({ json: windows });
  });
  await page.goto('/alerts/maintenance');
  const row = page.locator('.dt-row', { hasText: 'Core swap' });
  await row.getByRole('button', { name: 'Edit' }).click({ timeout: 15_000 });
  const dialog = page.getByRole('dialog');
  const startTime = dialog.getByRole('group', { name: 'Start time', exact: true });
  const endTime = dialog.getByRole('group', { name: 'End time', exact: true });
  await expect(startTime).toBeVisible();

  // The same columns: each date box and each time pair starts at the same x, at the same width.
  const box = async (l: ReturnType<typeof dialog.locator>) => {
    const b = await l.boundingBox();
    if (!b) throw new Error('not laid out');
    return [Math.round(b.x), Math.round(b.width)];
  };
  const startDateBox = dialog.locator('.wt-box', { has: page.locator('#maint-start-date') });
  const endDateBox = dialog.locator('.wt-box', { has: page.locator('#maint-end-date') });
  expect(await box(endDateBox)).toEqual(await box(startDateBox));
  expect(await box(endTime)).toEqual(await box(startTime));

  // 22:00 → 23:00 → 00:00 the next day; the 4 h 30 m window keeps its length.
  const later = startTime.getByRole('button', { name: '1 hour later' });
  await later.click();
  await expect(dialog.getByLabel('Start time, hour', { exact: true })).toHaveValue('23');
  await expect(dialog.getByLabel('End time, hour', { exact: true })).toHaveValue('03');
  await later.click();
  await expect(dialog.locator('#maint-start-date')).toHaveValue('2030-01-03');
  await expect(dialog.getByLabel('Start time, hour', { exact: true })).toHaveValue('00');
  await expect(dialog.getByLabel('End time, hour', { exact: true })).toHaveValue('04');
  await expect(dialog.getByText('Length 4h 30m', { exact: true })).toBeVisible();

  // A shortcut fills the end from the start; the minutes step to the next quarter.
  await dialog.getByRole('button', { name: '+2 h', exact: true }).click();
  await expect(dialog.getByLabel('End time, hour', { exact: true })).toHaveValue('02');
  await expect(dialog.getByLabel('End time, minute', { exact: true })).toHaveValue('00');
  await endTime.getByRole('button', { name: '15 minutes earlier' }).click();
  await expect(dialog.getByLabel('End time, hour', { exact: true })).toHaveValue('01');
  await expect(dialog.getByLabel('End time, minute', { exact: true })).toHaveValue('45');
  await expect(dialog.getByText('Length 1h 45m', { exact: true })).toBeVisible();

  const put = page.waitForRequest(
    (r) =>
      r.method() === 'PUT' &&
      new URL(r.url()).pathname === `/api/v1/maintenance-windows/${ID}/definition`,
  );
  await dialog.getByRole('button', { name: 'Save' }).click();
  const body = (await put).postDataJSON() as Record<string, string>;
  // 00:00 and 01:45 on 3 January in Tokyo.
  expect(body).toMatchObject({
    starts_at: '2030-01-02T15:00:00.000Z',
    ends_at: '2030-01-02T16:45:00.000Z',
    time_zone: 'Asia/Tokyo',
  });
  expect(errors.uncaught).toEqual([]);
});

test.describe('a new window', () => {
  // Fixed, so "the next whole hour" is the same on every machine that runs this.
  test.use({ timezoneId: 'Asia/Tokyo' });

  test('opens filled in, and its end never stays at or before its start', async ({
    page,
    errors,
  }) => {
    // ADR-261 Inc.7: from the next whole hour to an hour after it (decision 40). The end's ▼
    // stops short of the start, and an earlier time typed in the end lands on the next day once
    // the end is left - with the length counted that way while it is still being typed
    // (decision 41).
    await page.clock.setFixedTime(new Date('2030-01-02T05:20:00Z')); // 14:20 in Tokyo
    await page.goto('/alerts/maintenance');
    await page.getByRole('button', { name: '+ Add window', exact: true }).click({ timeout: 15_000 });
    const dialog = page.getByRole('dialog');
    const startHour = dialog.getByLabel('Start time, hour', { exact: true });
    const endHour = dialog.getByLabel('End time, hour', { exact: true });
    await expect(dialog.locator('#maint-start-date')).toHaveValue('2030-01-02');
    await expect(startHour).toHaveValue('15');
    await expect(dialog.getByLabel('Start time, minute', { exact: true })).toHaveValue('00');
    await expect(dialog.locator('#maint-end-date')).toHaveValue('2030-01-02');
    await expect(endHour).toHaveValue('16');
    await expect(dialog.getByText('Length 1h', { exact: true })).toBeVisible();

    // 16:00 an hour earlier would be the start itself: the press does nothing.
    const endTime = dialog.getByRole('group', { name: 'End time', exact: true });
    await endTime.getByRole('button', { name: '1 hour earlier' }).click();
    await expect(endHour).toHaveValue('16');

    // 14:00 typed for a 15:00 start: counted as the next day while typing, moved there on leaving.
    await endHour.fill('14');
    await expect(dialog.getByText('Length 23h', { exact: true })).toBeVisible();
    await expect(dialog.locator('#maint-end-date')).toHaveValue('2030-01-02');
    await dialog.getByLabel('End time, minute', { exact: true }).press('Tab');
    await expect(dialog.locator('#maint-end-date')).toHaveValue('2030-01-03');
    await expect(endHour).toHaveValue('14');
    await expect(dialog.getByText('Length 23h', { exact: true })).toBeVisible();

    // The start moves the end with it, keeping the 23 hours.
    await dialog
      .getByRole('group', { name: 'Start time', exact: true })
      .getByRole('button', { name: '1 hour later' })
      .click();
    await expect(startHour).toHaveValue('16');
    await expect(dialog.locator('#maint-end-date')).toHaveValue('2030-01-03');
    await expect(endHour).toHaveValue('15');
    expect(errors.uncaught).toEqual([]);
  });

  test('takes an hour typed through a Japanese input method', async ({ page, errors }) => {
    // The box used to read every composition update as typing, and a full-width digit is no
    // digit to `\D`: the box was emptied under the input method and the number never arrived.
    await page.clock.setFixedTime(new Date('2030-01-02T05:20:00Z')); // 14:20 in Tokyo
    await page.goto('/alerts/maintenance');
    await page.getByRole('button', { name: '+ Add window', exact: true }).click({ timeout: 15_000 });
    const dialog = page.getByRole('dialog');
    const startHour = dialog.getByLabel('Start time, hour', { exact: true });
    await startHour.click();
    const cdp = await page.context().newCDPSession(page);
    for (const text of ['２', '２２']) {
      await cdp.send('Input.imeSetComposition', {
        text,
        selectionStart: text.length,
        selectionEnd: text.length,
      });
    }
    await cdp.send('Input.insertText', { text: '２２' });
    await expect(startHour).toHaveValue('22');
    await expect(dialog.getByLabel('End time, hour', { exact: true })).toHaveValue('23');
    expect(errors.uncaught).toEqual([]);
  });
});

test.describe('a finger on the steppers', () => {
  test.use({ timezoneId: 'Asia/Tokyo', hasTouch: true });

  test('a held finger keeps stepping, and stops when it lifts', async ({ page, errors }) => {
    // ADR-261 decision 35: a held press repeats. A finger's mousedown and mouseup arrive together
    // after it lifts, so a stepper driven by mouse events stepped once for any length of hold.
    await page.clock.setFixedTime(new Date('2030-01-02T05:20:00Z')); // 14:20 in Tokyo
    await page.goto('/alerts/maintenance');
    await page.getByRole('button', { name: '+ Add window', exact: true }).click({ timeout: 15_000 });
    const dialog = page.getByRole('dialog');
    const endHour = dialog.getByLabel('End time, hour', { exact: true });
    await expect(endHour).toHaveValue('16');

    const up = dialog
      .getByRole('group', { name: 'End time', exact: true })
      .getByRole('button', { name: '1 hour later' });
    const b = await up.boundingBox();
    if (!b) throw new Error('the stepper has no box');
    // Through the DevTools protocol, so the page sees `pointerType: 'touch'`; Playwright's own
    // touchscreen only taps.
    const cdp = await page.context().newCDPSession(page);
    const at = { x: b.x + b.width / 2, y: b.y + b.height / 2 };
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [at] });
    // One step at once, then one every 90 ms after 400 ms: about five in 800 ms.
    await page.waitForTimeout(800);
    await cdp.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });

    const held = Number(await endHour.inputValue());
    expect(held, 'more than one step for a held finger').toBeGreaterThanOrEqual(18);
    await page.waitForTimeout(500);
    expect(Number(await endHour.inputValue()), 'no step after the finger lifts').toBe(held);
    expect(errors.uncaught).toEqual([]);
  });
});
