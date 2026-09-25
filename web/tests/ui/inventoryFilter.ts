// SPDX-License-Identifier: AGPL-3.0-only
// Driving the inventory tree's one filter button (ADR-177), for every spec that presses one of the
// switches or boxes behind it.
//
// One copy on purpose: before ADR-177 each spec found its control with `getByRole('button', …)`,
// and the four specs that press these would otherwise each learn the popover's markup on their own.

import type { Page } from '@playwright/test';

/** The funnel in the pane head. Its accessible name gains "(N in force)" while something is set. */
export const filterTrigger = (page: Page) =>
  page.getByRole('button', { name: /^Filter the inventory/ });

/** The popover the trigger opens. */
export const filterPopover = (page: Page) =>
  page.getByRole('dialog', { name: 'Filter the inventory' });

/** Open the popover unless it already is. */
export async function openInventoryFilter(page: Page): Promise<void> {
  if ((await filterPopover(page).count()) === 0) await filterTrigger(page).click();
}

/** A switch's input — what `toBeChecked` reads. It is drawn at 0×0 under its track, so a test
 *  presses the label ({@link pressInventorySwitch}) the way a person does. */
export const inventorySwitch = (page: Page, name: string) =>
  filterPopover(page).getByRole('switch', { name, exact: true });

/** Flip one of the Quick switches, opening the popover first when it is closed. */
export async function pressInventorySwitch(page: Page, name: string): Promise<void> {
  await openInventoryFilter(page);
  await filterPopover(page).locator('.invf-sw', { hasText: name }).click();
}

/** The chip row under the pane head, which says what is in force. */
export const filterChips = (page: Page) => page.getByRole('group', { name: 'Filters in force' });
