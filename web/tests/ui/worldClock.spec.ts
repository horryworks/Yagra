// SPDX-License-Identifier: AGPL-3.0-only
// The world clock (ADR-237): placed from the catalogue, it shows the viewer's zone and UTC; its ⚙
// adds a zone, refuses one that does not exist, and lays a card holding one zone out as one face.
// It reads no API, so nothing here is mocked beyond the empty board.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

const EMPTY_BOARD: Json = { version: 3, boards: [{ id: 'b1', name: 'Board', widgets: [] }] };

/** One card per layout the widget has: tiles (wide), rows (narrow), one face (one zone). */
const SEEDED_BOARD: Json = {
  version: 3,
  boards: [
    {
      id: 'b1',
      name: 'Board',
      widgets: [
        {
          instanceId: 'wide',
          type: 'world-clock',
          span: 8,
          settings: {
            zones: [
              { tz: 'local', label: '' },
              { tz: 'UTC', label: '' },
              { tz: 'America/New_York', label: '' },
              { tz: 'Europe/London', label: 'HQ' },
              { tz: 'Asia/Singapore', label: '' },
            ],
          },
        },
        { instanceId: 'solo', type: 'world-clock', span: 4, settings: { zones: [{ tz: 'UTC', label: '' }] } },
        {
          instanceId: 'narrow',
          type: 'world-clock',
          span: 4,
          rowSpan: 2,
          settings: {
            hour12: true,
            seconds: false,
            zones: [
              { tz: 'Asia/Tokyo', label: '' },
              { tz: 'America/Los_Angeles', label: '' },
              { tz: 'Asia/Kolkata', label: '' },
            ],
          },
        },
      ],
    },
  ],
};

test.use({ viewport: { width: 1440, height: 900 } });

test.describe('placed from the catalogue', () => {
  test.use({
    mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/dashboard': () => EMPTY_BOARD } },
  });

  test('shows the viewer and UTC, and its ⚙ adds a zone or says why not', async ({ page, errors }) => {
    await page.goto('/dashboard/my');
    await page.getByRole('button', { name: 'Customize' }).click();
    await page.getByRole('button', { name: 'Add widget' }).click();
    const item = page.locator('.catalog-item:has(.catalog-item-title:text-is("World clock"))');
    await expect(item).toHaveCount(1);
    const section = item.locator('xpath=ancestor::div[contains(@class,"catalog-section")]');
    await expect(section.getByRole('heading')).toHaveText(/general/i);
    await item.click();
    await page.locator('.modal').getByRole('button', { name: 'Done' }).click();

    const cell = page.locator('.mydash-cell').first();
    await expect(cell.locator('.wclock-zone')).toHaveCount(2);
    await expect(cell.locator('.wclock-label')).toHaveText(['Local', 'UTC']);
    await expect(cell.locator('.wclock-time').first()).toHaveText(/^\d\d:\d\d:\d\d$/);

    await cell.getByRole('button', { name: /settings/i }).click();
    const box = page.getByRole('combobox', { name: 'Time zone to add' });
    await box.fill('mars/base');
    await page.getByRole('button', { name: 'Add', exact: true }).click();
    await expect(page.locator('.field-error')).toHaveText('Unknown time zone');

    await box.fill('asia/tokyo');
    await box.press('Enter');
    await expect(page.locator('.field-error')).toHaveCount(0);
    await expect(cell.locator('.wclock-label')).toHaveText(['Local', 'UTC', 'Tokyo']);
    await expect(page.locator('.wclock-count')).toHaveText('3 / 8');

    await box.fill('UTC');
    await box.press('Enter');
    await expect(page.locator('.field-error')).toHaveText('Already on this card');

    // Kept in the spelling it was given (ADR-253): Chromium's Intl would have renamed it Calcutta.
    await box.fill('asia/kolkata');
    await box.press('Enter');
    await expect(cell.locator('.wclock-label')).toHaveText(['Local', 'UTC', 'Tokyo', 'Kolkata']);

    expect(errors.uncaught).toEqual([]);
  });
});

test.describe('on a board holding all three layouts', () => {
  test.use({
    mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/dashboard': () => SEEDED_BOARD } },
  });

  test('tiles, rows and one face each fit their card', async ({ page, errors }) => {
    await page.goto('/dashboard/my');
    const cells = page.locator('.mydash-cell');
    await expect(cells).toHaveCount(3);

    // Wide: tiles side by side, the operator's label in place of the city.
    const wide = cells.nth(0);
    await expect(wide.locator('.wclock-label')).toHaveText(['Local', 'UTC', 'New York', 'HQ', 'Singapore']);
    const tops = await wide.locator('.wclock-zone').evaluateAll((els) =>
      els.map((e) => Math.round(e.getBoundingClientRect().top)),
    );
    expect(new Set(tops).size, 'five tiles on one row at 8 columns').toBe(1);

    // One zone: the single large face.
    await expect(cells.nth(1).locator('.wclock-solo .wclock-time')).toHaveText(/^\d\d:\d\d:\d\d$/);

    // Narrow, 12-hour, no seconds: one row per zone.
    const narrow = cells.nth(2);
    await expect(narrow.locator('.wclock-time').first()).toHaveText(/^\d\d:\d\d(AM|PM)$/);
    await expect(narrow.locator('.wclock-meta').nth(2)).toContainText('UTC+5:30');

    // Nothing cut off or overflowing in any card.
    for (let i = 0; i < 3; i++) {
      const body = cells.nth(i).locator('.card-body');
      const [sw, cw] = await body.evaluate((b) => [b.scrollWidth, b.clientWidth]);
      expect(sw, `card ${i} scrolls sideways`).toBeLessThanOrEqual(cw);
    }

    if (process.env.WCLOCK_SHOTS) {
      await page.screenshot({ path: `${process.env.WCLOCK_SHOTS}/board.png` });
      await page.emulateMedia({ colorScheme: 'dark' });
      await page.screenshot({ path: `${process.env.WCLOCK_SHOTS}/board-dark.png` });
    }
    expect(errors.uncaught).toEqual([]);
  });
});

/** A wide card whose tiles carry "Yesterday" or "Tomorrow". The viewer is pinned to Tokyo (UTC+9):
 *  Pago Pago (UTC-11) is the day before until 20:00 there and Kiritimati (UTC+14) the day after from
 *  19:00, so at least one shift is on screen whatever the time of the run. */
const SHIFTED_BOARD: Json = {
  version: 3,
  boards: [
    {
      id: 'b1',
      name: 'Board',
      widgets: [
        {
          instanceId: 'shifted',
          type: 'world-clock',
          span: 8,
          settings: {
            zones: ['UTC', 'America/New_York', 'America/Los_Angeles', 'Pacific/Pago_Pago', 'Pacific/Kiritimati'].map(
              (tz) => ({ tz, label: '' }),
            ),
          },
        },
      ],
    },
  ],
};

test.describe('a tile whose date differs from the viewer', () => {
  test.use({
    timezoneId: 'Asia/Tokyo',
    mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/dashboard': () => SHIFTED_BOARD } },
  });

  test('keeps Yesterday and Tomorrow inside the tile', async ({ page, errors }) => {
    // 2026-11-03, measured: "Tue, 11/3 · UTC-8 · Yesterday" on one unbreakable line ran out of a
    // 150px tile and into the next. The card body did not scroll, so the check above missed it.
    await page.goto('/dashboard/my');
    const zones = page.locator('.wclock-zone');
    await expect(zones).toHaveCount(5);
    await expect(page.locator('.wclock-shift').first()).toBeVisible();

    const spill = await zones.evaluateAll((els) =>
      els.flatMap((zone) => {
        const box = zone.getBoundingClientRect();
        return [...zone.querySelectorAll<HTMLElement>('.wclock-when, .wclock-shift, .wclock-label')]
          .filter((el) => el.getBoundingClientRect().right > box.right + 0.5)
          .map((el) => `${el.className}: ${el.textContent}`);
      }),
    );
    expect(spill, 'text running past its tile').toEqual([]);
    if (process.env.WCLOCK_SHOTS) {
      await page.locator('.mydash-cell').first().screenshot({ path: `${process.env.WCLOCK_SHOTS}/shifted.png` });
    }
    expect(errors.uncaught).toEqual([]);
  });
});
