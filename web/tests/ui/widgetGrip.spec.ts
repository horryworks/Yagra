// SPDX-License-Identifier: AGPL-3.0-only
// The resize grip is never under a card's scrollbar. A classic scrollbar runs the full height of a
// body that overflows and is painted over the grip's corner, so the grip on a scrolling card was
// there and draggable but invisible (2026-11-03, a world clock holding six zones). While
// customizing, the body therefore ends above the grip.
//
// Asserted as geometry, because the walk runs with scrollbars hidden (`--hide-scrollbars` is
// Playwright's default) and so cannot see the paint order at all: if the body's box stops above the
// grip, no scrollbar inside that box can reach it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

const ZONES = [
  'local',
  'Asia/Shanghai',
  'UTC',
  'America/New_York',
  'America/Los_Angeles',
  'Europe/London',
].map((tz) => ({ tz, label: '' }));

/** One card whose content overflows its height and one that fits. */
const BOARD: Json = {
  version: 3,
  boards: [
    {
      id: 'b1',
      name: 'Board',
      widgets: [
        { instanceId: 'over', type: 'world-clock', span: 4, settings: { zones: ZONES } },
        { instanceId: 'fits', type: 'world-clock', span: 4, settings: { zones: [{ tz: 'UTC', label: '' }] } },
      ],
    },
  ],
};

test.use({
  viewport: { width: 1440, height: 900 },
  launchOptions: { ignoreDefaultArgs: ['--hide-scrollbars'] },
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/dashboard': () => BOARD } },
});

test('while customizing, no card body reaches down to its resize grip', async ({ page, errors }) => {
  await page.goto('/dashboard/my');
  await page.getByRole('button', { name: 'Customize' }).click();
  const cells = page.locator('.mydash-cell');
  await expect(cells).toHaveCount(2);

  const measured = await cells.evaluateAll((els) =>
    els.map((cell) => {
      const body = cell.querySelector('.card-body') as HTMLElement;
      const grip = cell.querySelector('.widgetframe-resize') as HTMLElement;
      return {
        overflows: body.scrollHeight > body.clientHeight,
        bodyBottom: body.getBoundingClientRect().bottom,
        gripTop: grip.getBoundingClientRect().top,
      };
    }),
  );
  // The case this exists for has to be present, or the check is about a card that never scrolled.
  expect(measured[0].overflows, 'the six-zone card overflows its height').toBe(true);
  for (const m of measured) expect(m.bodyBottom).toBeLessThanOrEqual(m.gripTop);

  // And the grip on the scrolling card still resizes it.
  const grip = page.locator('.widgetframe-resize').first();
  const b = (await grip.boundingBox())!;
  const before = (await cells.first().boundingBox())!.height;
  await page.mouse.move(b.x + b.width / 2, b.y + b.height / 2);
  await page.mouse.down();
  await page.mouse.move(b.x + b.width / 2, b.y + 300, { steps: 8 });
  await page.mouse.up();
  await expect.poll(async () => (await cells.first().boundingBox())!.height).toBeGreaterThan(before);

  expect(errors.uncaught).toEqual([]);
});
