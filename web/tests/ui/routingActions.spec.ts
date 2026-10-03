// SPDX-License-Identifier: AGPL-3.0-only
// Notification delivery: every row action is pressable, and a routing rule can be edited (ADR-193).
//
// The first half is the walk's row-action check pinned to the screen where it was found. The
// channel table grew a fourth button in ADR-192 while its actions column stayed 96px, so "Disable"
// and "Delete" sat past the cell's edge — opaque, boxed, and unreachable. The walk now hit-tests
// every table; this names the screen and the number, so a selector that stopped finding buttons
// cannot pass here with zero.

import { expect, test } from '../support/app';
import { inspectRowActions } from './rowActions';

test('every row action on Notification delivery can be pressed', async ({ page }) => {
  await page.goto('/alerts/routing');
  await expect(page.locator('.dt-row').first()).toBeVisible();
  const report = await inspectRowActions(page);
  expect(report.findings).toEqual([]);
  // Five on a channel (test, delivery log, template, on/off, delete — ADR-195 added the log) and
  // three on a rule (edit, on/off, delete).
  expect(report.buttons).toBe(8);
});

test('a routing rule opens for editing with its own values', async ({ page }) => {
  await page.goto('/alerts/routing');
  const ruleRow = page.locator('.routing-rules-section .dt-row').first();
  await ruleRow.hover();
  await ruleRow.getByRole('button', { name: 'Edit rule' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('heading')).toContainText('Edit routing rule');
  // The name field carries the rule's own name rather than starting blank.
  const name = await ruleRow.locator('.yt-name-txt').innerText();
  await expect(dialog.locator('input[type="text"], input:not([type])').first()).toHaveValue(name);
  await expect(dialog.getByRole('button', { name: 'Save' })).toBeVisible();
});
