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
  // three on a rule (edit, on/off, delete) and two on a grouping exception (edit, remove). The
  // delivery-hours table is gone: a rule keeps its own hours (ADR-262).
  expect(report.buttons).toBe(10);
});

// Refresh used to write the same filters back to the URL. The query string did not change, so
// `useSearchParams` returned the same object and the log was never asked again. If it regresses,
// `waitForRequest` times out instead of passing on nothing.
test('Refresh on the delivery log asks the server again', async ({ page }) => {
  const first = page.waitForRequest((r) => r.url().includes('/api/v1/notification-deliveries'));
  await page.goto('/alerts/deliveries');
  await first;
  const again = page.waitForRequest((r) => r.url().includes('/api/v1/notification-deliveries'));
  await page.getByRole('button', { name: 'Refresh' }).click();
  await again;
});

// ADR-262 decision 7: the log is its own page, and a channel row opens it narrowed to that channel
// - the server is asked for that channel's rows, not the whole log.
test('a channel row opens the delivery log narrowed to that channel', async ({ page }) => {
  await page.goto('/alerts/routing');
  const channelRow = section(page, 'Notification channels').locator('.dt-row').first();
  await channelRow.hover();
  const narrowed = page.waitForRequest(
    (r) => r.url().includes('/api/v1/notification-deliveries') && r.url().includes('channel='),
  );
  await channelRow.getByRole('button', { name: 'Show delivery log' }).click();
  await expect(page).toHaveURL(/\/alerts\/deliveries\?channel=/);
  await narrowed;
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

// ADR-259: the rule's tags are edited in the dialog and sent with the rest of its definition, so a
// save from the screen can never drop them by leaving the field out.
test('a routing rule saves the tags it was given', async ({ page }) => {
  await page.goto('/alerts/routing');
  const ruleRow = section(page, 'Routing rules').locator('.dt-row').first();
  await ruleRow.hover();
  await ruleRow.getByRole('button', { name: 'Edit rule' }).click();
  const dialog = page.getByRole('dialog');
  // Inc.2: the tags are a list of boxes, one per tag nodes carry, so nothing can be typed wrong.
  // Decision 17: a rule with tags opens on "Only these"; the boxes show only there. Since ADR-262
  // the folders are a second list beside them, in the one "Applies to" group.
  const target = dialog.getByRole('group', { name: 'Applies to', exact: true });
  await expect(target).toBeVisible();
  await expect(target.getByRole('button', { name: 'Only these' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  // The generated rule may name a folder too; take it away so the tags alone decide the reach.
  const folders = target.getByRole('group', { name: 'Folders', exact: true });
  for (const box of await folders.locator('.checklist-rows input:checked').all()) {
    await box.uncheck();
  }
  const tags = target.getByRole('group', { name: 'Tags', exact: true });
  // The rule's own tag no node carries is listed first, ticked and marked.
  const gone = tags.locator('.checklist-row', { hasText: 'no node' });
  await expect(gone).toHaveCount(1);
  await expect(gone.getByRole('checkbox')).toBeChecked();
  await gone.getByRole('checkbox').uncheck();
  // Unticked, it stays on the list rather than vanishing under the pointer.
  await expect(gone).toHaveCount(1);
  // Nothing ticked under "Only these tags" reaches no node, and the rule cannot be saved.
  await expect(dialog.getByText('Reaches 0 nodes')).toBeVisible();
  await expect(dialog.getByRole('button', { name: 'Save' })).toBeDisabled();
  const teamA = tags.locator('.checklist-row', { hasText: 'TeamA' });
  await expect(teamA).toContainText('3 nodes');
  await teamA.getByRole('checkbox').check();
  await expect(dialog.getByText('Reaches 3 nodes')).toBeVisible();
  const saved = page.waitForRequest(
    (r) => r.method() === 'PUT' && r.url().includes('/definition'),
  );
  await dialog.getByRole('button', { name: 'Save' }).click();
  const body = (await saved).postDataJSON() as { tags?: unknown; group_ids?: unknown };
  expect(Array.isArray(body.tags)).toBe(true);
  expect(body.tags).toEqual(['TeamA']);
  expect(body.group_ids).toEqual([]);
});

// Inc.2: a rule naming a tag no node carries matches nothing, so its row says so.
test('a rule tag no node carries is marked on the rule list', async ({ page }) => {
  await page.goto('/alerts/routing');
  const ruleRow = section(page, 'Routing rules').locator('.dt-row').first();
  await expect(ruleRow.locator('.badge', { hasText: 'no node' })).toBeVisible();
});

// ADR-262 decision 3: a rule keeps its own hours. "During" shows the weekdays and the two times,
// and the window is sent with the rest of the rule.
test("a routing rule's hours are chosen in its dialog and sent with it", async ({ page }) => {
  await page.goto('/alerts/routing');
  const ruleRow = section(page, 'Routing rules').locator('.dt-row').first();
  await ruleRow.hover();
  await ruleRow.getByRole('button', { name: 'Edit rule' }).click();
  const dialog = page.getByRole('dialog');
  const when = dialog.getByRole('group', { name: 'When', exact: true });
  await when.getByRole('button', { name: 'During', exact: true }).click();
  await expect(when.getByRole('checkbox')).toHaveCount(7);
  const saved = page.waitForRequest((r) => r.method() === 'PUT' && r.url().includes('/definition'));
  await dialog.getByRole('button', { name: 'Save' }).click();
  const body = (await saved).postDataJSON() as { when?: { mode: string; days: number[] } };
  expect(body.when?.mode).toBe('during');
  expect(body.when?.days.length).toBeGreaterThan(0);
});

// ADR-262 decision 4: the catch-all is the line under the rules, and its channels are set there.
test('the catch-all line sets the channels an alert goes to when no rule is open', async ({ page }) => {
  await page.goto('/alerts/routing');
  await page.getByRole('button', { name: 'Set channels' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.locator('.checklist-rows input').first().check();
  const saved = page.waitForRequest(
    (r) => r.method() === 'PUT' && r.url().endsWith('/api/v1/routing-catch-all'),
  );
  await dialog.getByRole('button', { name: 'Save' }).click();
  const body = (await saved).postDataJSON() as { channel_ids: string[] };
  expect(body.channel_ids.length).toBeGreaterThan(0);
});

// ADR-262 decision 8: the coverage week draws a row per set of rules, seven day bars each, and
// asks again for another severity.
test('the coverage week draws its rows and redraws for another severity', async ({ page }) => {
  const first = page.waitForRequest((r) => r.url().includes('/api/v1/routing-coverage'));
  await page.goto('/alerts/routing');
  expect(new URL((await first).url()).searchParams.get('severity')).toBe('critical');
  const coverage = page.locator('.routing-coverage');
  await expect(coverage.locator('.coverage-day').first()).toBeVisible();
  expect(await coverage.locator('.coverage-row').count()).toBeGreaterThan(1);
  const again = page.waitForRequest(
    (r) => r.url().includes('/api/v1/routing-coverage') && r.url().includes('severity=warning'),
  );
  await coverage.getByRole('button', { name: 'Warning', exact: true }).click();
  await again;
});
