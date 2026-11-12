// SPDX-License-Identifier: AGPL-3.0-only
// Moving a row of a reorderable list the way a person does (ADR-255): by its ⠿ grip, which is the
// only place a drag starts. The keyboard path is the deterministic one; `dragByMouse` is for the
// test that has to prove a pointer drag works at all.

import { expect, type Locator, type Page } from '@playwright/test';

/** The grip of the row named `name`, inside `scope` (the page when omitted). */
export function grip(scope: Page | Locator, name: string): Locator {
  return scope.getByRole('button', { name: `Reorder ${name}`, exact: true });
}

/** Pick the row up with Space, move it `steps` places (negative is up), and drop it with Space.
 *
 *  ⚠️ It waits as a person would. A key pressed before dnd-kit has measured the list after the
 *  pick-up moves nothing, and a pick-up during the previous drop's animation is not taken — both
 *  were seen, as a drop at the row's own place and no save at all. */
export async function dragByKeys(page: Page, scope: Page | Locator, name: string, steps: number): Promise<void> {
  const lifted = page.locator('.is-dragging');
  await expect(lifted).toHaveCount(0);
  await grip(scope, name).focus();
  await page.keyboard.press('Space');
  await expect(lifted).toHaveCount(1);
  for (let i = 0; i < Math.abs(steps); i += 1) {
    await page.keyboard.press(steps < 0 ? 'ArrowUp' : 'ArrowDown');
    await page.waitForTimeout(80);
  }
  await page.keyboard.press('Space');
  await expect(lifted).toHaveCount(0);
}

/** Drag the row named `name` by its grip with the mouse, onto the row `target` stands in. */
export async function dragByMouse(page: Page, scope: Page | Locator, name: string, target: Locator): Promise<void> {
  const from = await grip(scope, name).boundingBox();
  const to = await target.boundingBox();
  if (!from || !to) throw new Error(`dragByMouse: ${name} or its target is not on screen`);
  const x = from.x + from.width / 2;
  await page.mouse.move(x, from.y + from.height / 2);
  await page.mouse.down();
  // Past the 4px a drag needs before it starts, then onto the target in steps the sensor sees.
  await page.mouse.move(x, from.y + from.height / 2 + (to.y > from.y ? 8 : -8));
  await page.mouse.move(x, to.y + to.height / 2, { steps: 12 });
  await page.mouse.up();
}
