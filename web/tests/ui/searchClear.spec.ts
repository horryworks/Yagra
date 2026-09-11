// SPDX-License-Identifier: AGPL-3.0-only
// Emptying a search box from the box itself (ADR-132), in a browser.
//
// WHY TIER1. Everything here is either geometry or wiring, and the Vitest guard beside it
// (`src/searchFields.test.ts`) can only read text:
//
//   - 🚨 **The ✕ acts on `click` but prevents default on `mousedown`.** Anything floating above the
//     page dismisses on *mousedown* (`AnchoredPopover`), so the naive version is a button whose
//     click can arrive after its own surface has gone. The proof is that the caret is still in the
//     box afterwards and the popover is still open — neither is visible to a source scan, and
//     neither is visible to a unit test of a component Vitest never executes.
//   - **The ✕ has to be inside the box.** It is absolutely positioned against the wrapper while the
//     width lives on the wrapper and the border on the input, so getting the split wrong puts the ✕
//     next to the box instead of in it. Only real boxes can say.
//   - ⚠️ **`isVisible()` is not used**, per ADR-088: it answers true for an `opacity: 0` element.
//     Where this asks whether the operator can see the ✕, it reads the computed opacity.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

const NEEDLE = 'router';

/** The ✕ inside a box, by its accessible name — the same string the component takes from
 *  `common:actions.clearSearch`. */
const CLEAR = 'Clear search';

/** Where the ✕ sits relative to the input it clears, and whether it can be seen. */
async function insideTheBox(
  input: import('@playwright/test').Locator,
  clear: import('@playwright/test').Locator,
) {
  const [box, x] = [await input.boundingBox(), await clear.boundingBox()];
  expect(box, 'the input has no box').not.toBeNull();
  expect(x, 'the ✕ has no box').not.toBeNull();
  if (!box || !x) return;
  expect(x.x, 'the ✕ is left of the box it belongs to').toBeGreaterThan(box.x);
  expect(
    x.x + x.width,
    'the ✕ sticks out past the right edge of its own box — the width is on the wrong element',
  ).toBeLessThanOrEqual(box.x + box.width + 1);
  expect(x.width, 'the ✕ has no width').toBeGreaterThan(8);
  const opacity = await clear.evaluate((el) => getComputedStyle(el).opacity);
  expect(opacity, 'the ✕ is transparent — nobody can find it').toBe('1');
}

/** Whether the element holding the caret is the one passed in. */
const holdsCaret = (input: import('@playwright/test').Locator) =>
  input.evaluate((el) => el === document.activeElement);

test('the top bar search clears itself and keeps the caret', async ({ page }) => {
  await page.goto('/nodes');
  const input = page.getByRole('combobox', { name: 'Global search' });
  await expect(input).toHaveCount(1);

  // Nothing typed, nothing to clear: a ✕ that is always there is a control that does nothing half
  // the time, and this is the half.
  await expect(page.locator('.gsearch').getByRole('button', { name: CLEAR })).toHaveCount(0);

  await input.fill(NEEDLE);
  const clear = page.locator('.gsearch').getByRole('button', { name: CLEAR });
  await expect(clear).toHaveCount(1);
  await insideTheBox(input, clear);

  await clear.click();
  await expect(input).toHaveValue('');
  await expect(clear).toHaveCount(0);
  expect(
    await holdsCaret(input),
    'pressing the ✕ took the caret out of the box — the mousedown default was not prevented',
  ).toBe(true);
  // The results popover opens on focus and closes on an outside mousedown. Clearing is "start
  // again", so it must still be open — and if the caret had left the box it would not be.
  await expect(page.locator('.gsearch-pop')).toHaveCount(1);
});

test('the node tree search clears itself, and only itself', async ({ page }) => {
  await page.goto('/nodes');
  const input = page.getByRole('searchbox', { name: 'Search' });
  await expect(input).toHaveCount(1);

  // The count is a floor on what this file inspected: two boxes are reachable on this screen, and a
  // locator that stopped matching one of them would otherwise report a clean screen.
  await expect(page.locator('.sfield'), 'this screen has two search boxes').toHaveCount(2);

  await input.fill(NEEDLE);
  const clear = page.locator('.nodes-pane-search').getByRole('button', { name: CLEAR });
  await expect(clear).toHaveCount(1);
  await insideTheBox(input, clear);

  // The three inventory controls are URL state and this box is not. The ✕ writes neither of the
  // other two, which is why it calls `setFilter('')` and not `clearAllFilters` — one handler, one
  // URL write, and this handler makes none.
  const before = page.url();
  await clear.click();
  await expect(input).toHaveValue('');
  expect(page.url(), 'clearing the box wrote the URL').toBe(before);
  expect(await holdsCaret(input), 'the ✕ took the caret out of the box').toBe(true);
});

test.describe('inside a column filter', () => {
  test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

  test('the box clears its own term and the popover stays open', async ({ page }) => {
    await page.goto('/events');
    await expect(page.getByRole('group', { name: 'Column filters' })).toBeVisible();

    await page.getByRole('button', { name: /Filter by Message/ }).click();
    const panel = page.getByRole('dialog');
    await expect(panel).toBeVisible();
    const input = panel.getByRole('searchbox').first();
    await input.fill(NEEDLE);
    await expect(page, 'the term never reached the URL').toHaveURL(new RegExp(NEEDLE));

    // Two ✕ are on screen and they are different controls: this one empties the box, the one beside
    // the closed trigger drops the whole condition. The second is `ColumnFilterCell`'s own and
    // predates this ADR.
    const inBox = panel.getByRole('button', { name: CLEAR });
    await expect(inBox).toHaveCount(1);
    await expect(page.getByRole('button', { name: /Clear the Message filter/ })).toHaveCount(1);
    await insideTheBox(input, inBox);

    await inBox.click();
    await expect(input).toHaveValue('');
    // 🚨 The failure this test exists for: the popover dismisses on mousedown, so a ✕ that let the
    // caret leave the box would be pressed against a panel that had already gone.
    await expect(panel, 'the popover closed when the ✕ was pressed').toBeVisible();
    expect(await holdsCaret(input), 'the ✕ took the caret out of the box').toBe(true);
    await expect(page, 'clearing the box left the term in the URL').not.toHaveURL(
      new RegExp(NEEDLE),
    );
  });
});
