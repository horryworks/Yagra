// SPDX-License-Identifier: AGPL-3.0-only
// Notification delivery: every row action is pressable, and a routing rule can be edited (ADR-193).
//
// The first half is the walk's row-action check pinned to the screen where it was found. The
// channel table grew a fourth button in ADR-192 while its actions column stayed 96px, so "Disable"
// and "Delete" sat past the cell's edge — opaque, boxed, and unreachable. The walk now hit-tests
// every table; this names the screen and the number, so a selector that stopped finding buttons
// cannot pass here with zero.

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';
import { inspectRowActions } from './rowActions';

/** One section of the screen, found by its heading. */
const section = (page: Page, title: string) =>
  page.locator('section').filter({ has: page.getByRole('heading', { name: title, exact: true }) });

test('every row action on Notification delivery can be pressed', async ({ page }) => {
  await page.goto('/alerts/routing');
  await expect(page.locator('.dt-row').first()).toBeVisible();
  const report = await inspectRowActions(page);
  expect(report.findings).toEqual([]);
  // Five on a channel (test, delivery log, template, on/off, delete — ADR-195 added the log),
  // three on a rule (edit, on/off, delete), one on a folder's grouping level (ADR-236) and three
  // on a delivery-hours schedule (ADR-226).
  expect(report.buttons).toBe(12);
});

// Refresh used to write the same filters back to the URL. The query string did not change, so
// `useSearchParams` returned the same object and the log was never asked again. If it regresses,
// `waitForRequest` times out instead of passing on nothing.
test('Refresh on the delivery log asks the server again', async ({ page }) => {
  const first = page.waitForRequest((r) => r.url().includes('/api/v1/notification-deliveries'));
  await page.goto('/alerts/routing');
  await first;
  const log = section(page, 'Delivery log');
  await expect(log).toBeVisible();
  const again = page.waitForRequest((r) => r.url().includes('/api/v1/notification-deliveries'));
  await log.getByRole('button', { name: 'Refresh' }).click();
  await again;
});

test('a routing rule opens for editing with its own values', async ({ page }) => {
  await page.goto('/alerts/routing');
  const ruleRow = section(page, 'Routing rules').locator('.dt-row').first();
  await ruleRow.hover();
  await ruleRow.getByRole('button', { name: 'Edit rule' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('heading')).toContainText('Edit routing rule');
  // The name field carries the rule's own name rather than starting blank.
  const name = await ruleRow.locator('.yt-name-txt').innerText();
  await expect(dialog.locator('input[type="text"], input:not([type])').first()).toHaveValue(name);
  await expect(dialog.getByRole('button', { name: 'Save' })).toBeVisible();
});
