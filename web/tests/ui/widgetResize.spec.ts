// SPDX-License-Identifier: AGPL-3.0-only
// Resizing the last widget on a board — the case a card at the bottom of the scroller cannot do.
//
// Reported from the running deployment: a widget placed at the foot of a board could not be made
// taller. The grip resizes by dragging DOWN, and three things conspire at the bottom:
//   * the scroller is `main.shell-content`, not the window;
//   * `onPointerDown` calls `preventDefault` and captures the pointer, so nothing scrolls under a
//     drag — by design, or the board would scroll instead of resizing;
//   * the grid ended flush with its content, so at maximum scroll the grip sat two pixels above the
//     scroller's bottom edge with a full row (240px) of travel needed to reach the next size.
// A real pointer cannot leave the window, so the gesture was simply unavailable down there.
//
// ⚠️ These tests must move the mouse only to coordinates a person could reach. Playwright will
// happily dispatch a move to y = 900 in a 560px window, and that passes with the bug still present:
// it models a mouse that can leave the screen. The bound is the viewport, and it is the assertion.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

/** Three full-width cards, all of them resizable in height: enough that the last one is below the
 *  fold in a short window.
 *
 *  ⚠️ The type matters. Most of the catalogue declares no `allowedRowSpans` and is therefore
 *  fixed-height by design — `snapSize` locks those to 1 whatever the pointer does, so a test built
 *  on one fails identically before and after any fix and proves nothing. `active-alerts` and
 *  `alert-volume` opt into taller sizes. */
const BOARD = {
  version: 3,
  boards: [
    {
      id: 'b1',
      name: 'Board',
      widgets: [
        { instanceId: 'w1', type: 'active-alerts', span: 12 },
        { instanceId: 'w2', type: 'alert-volume', span: 12 },
        { instanceId: 'w3', type: 'active-alerts', span: 12 },
      ],
    },
  ],
};

test.use({
  viewport: { width: 1440, height: 560 },
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/dashboard': () => BOARD } },
});

/** Enter Customize and scroll the board to its end — where the reported bug lives. */
async function customizeAndScrollToEnd(page: import('@playwright/test').Page) {
  await page.goto('/dashboard/my');
  await page.getByRole('button', { name: 'Customize' }).click();
  await expect(page.locator('.mydash-cell').last()).toBeAttached();
  await page.evaluate(() => {
    const s = document.querySelector('.shell-content') as HTMLElement;
    s.scrollTop = s.scrollHeight;
  });
  // The reflow the scroll causes has to settle before anything is measured off it.
  await page.waitForTimeout(100);
}

test('the last widget on a board can still be made taller', async ({ page }) => {
  await customizeAndScrollToEnd(page);

  const cell = page.locator('.mydash-cell').last();
  await expect(cell).toHaveClass(/mydash-rowspan-1/);

  const grip = cell.locator('.widgetframe-resize');
  const g = (await grip.boundingBox())!;
  const vh = page.viewportSize()!.height;

  // The grip has to be reachable in the first place.
  expect(g.y, 'the resize grip is below the fold').toBeLessThan(vh);

  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await page.mouse.down();
  // Straight down to the last row of pixels a mouse can occupy — no further, because there is no
  // further. Several steps so the auto-scroll has frames to run in.
  await page.mouse.move(g.x + g.width / 2, vh - 1, { steps: 20 });
  // Hold at the edge: the fix scrolls the board under the pointer, and that takes frames the
  // gesture would not otherwise supply.
  await page.waitForTimeout(600);
  await page.mouse.up();

  await expect(cell, 'dragging to the bottom edge did not make the card taller').toHaveClass(
    /mydash-rowspan-[23]/,
  );
});

test('a resize drag still commits the size the pointer asked for', async ({ page }) => {
  // The receiving half: the fix scrolls the board during the drag, and a delta measured in window
  // coordinates would then count that scroll twice — a small drag would jump two steps. This is
  // the same gesture nowhere near an edge, where no scrolling happens and the answer is known.
  await page.goto('/dashboard/my');
  await page.getByRole('button', { name: 'Customize' }).click();

  const cell = page.locator('.mydash-cell').first();
  await expect(cell).toHaveClass(/mydash-rowspan-1/);
  const g = (await cell.locator('.widgetframe-resize').boundingBox())!;

  // One row is 240px; 150px is past the half-row snap and nowhere near two rows.
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2);
  await page.mouse.down();
  await page.mouse.move(g.x + g.width / 2, g.y + g.height / 2 + 150, { steps: 10 });
  await page.mouse.up();

  await expect(cell).toHaveClass(/mydash-rowspan-2/);
  await expect(cell, 'the drag overshot by a whole row').not.toHaveClass(/mydash-rowspan-3/);
});
