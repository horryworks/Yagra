// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Threshold rules: the level tabs and Check a node (ADR-230; the node view was a node-detail
// tab under ADR-229, Tier1).
//
// Why Tier1: the grouping and which rule the button edits are decided in `nodeRules.ts`, and the
// view's URL in `thresholdQuery.ts`, which Vitest runs. What only a browser proves is the wiring:
// `?check=` reads the node's route, a metric row shows the rule in force with where it was decided,
// opening the row shows the rule it beat, the button opens the rule dialog at the node level, and
// a level tab writes the URL and brings its target picker.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** A folder rule in force on `cpu_util`, beating a profile rule on the same metric. Built from the
 *  generated shape so a change to the Rust type reaches this fixture. */
function nodeRules(): Json {
  const template = (defaultBodyFor('/api/v1/thresholds') as Record<string, Json>)
    .items as Record<string, Json>[];
  const [rule] = template;
  const bounds = (warning: number, critical: number) => ({
    direction: 'above',
    warning,
    critical,
    warning_below: null,
    critical_below: null,
    warning_above: warning,
    critical_above: critical,
    dwell_samples: 3,
    row_match: null,
  });
  return [
    {
      rule: {
        ...rule,
        id: '00000000-0000-4000-8000-00000000f001',
        scope_level: 'group_id',
        scope_ids: ['00000000-0000-4000-8000-00000000f0f0'],
        metric: 'cpu_util',
        ...bounds(70, 85),
      },
      in_force: true,
    },
    {
      rule: {
        ...rule,
        id: '00000000-0000-4000-8000-00000000f002',
        scope_level: 'profile',
        scope_ids: ['00000000-0000-4000-8000-00000000f0f1'],
        metric: 'cpu_util',
        ...bounds(80, 95),
      },
      in_force: false,
    },
  ] as unknown as Json;
}

test.use({
  viewport: { width: 1280, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/thresholds': () => nodeRules(),
    },
  },
});

test('a metric row shows the rule in force, the rule it beat, and starts a rule for this node', async ({
  page,
  errors,
}) => {
  await page.goto(`/alerts/rules?check=${NODE_ID}`);
  const row = page.locator('.dt-row', { hasText: 'cpu_util' });
  await expect(row).toBeVisible({ timeout: 15_000 });
  // The bounds in force are the folder's, not the profile's.
  await expect(row).toContainText('70');
  await expect(row).toContainText('folder group');
  await expect(row).not.toContainText('95');

  // Opening the row shows what it beat.
  await row.click();
  const beaten = page.locator('.nd-rules-overridden li');
  await expect(beaten).toHaveCount(1);
  await expect(beaten).toContainText('95');

  // The button starts a node-level rule from the bounds in force.
  await row.getByRole('button', { name: 'Change for this node' }).click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('input[value="70"]')).toHaveCount(1);
  expect(errors.uncaught).toEqual([]);
});

test('a level tab narrows the list in the URL and brings its target picker', async ({ page }) => {
  await page.goto('/alerts/rules');
  const tabs = page.getByRole('tablist');
  await expect(tabs).toBeVisible({ timeout: 15_000 });
  await expect(tabs.getByRole('tab', { name: 'All', exact: true })).toHaveAttribute(
    'aria-selected',
    'true',
  );
  await tabs.getByRole('tab', { name: 'Profile', exact: true }).click();
  await expect(page).toHaveURL(/scope_level=profile/);
  await expect(page.locator('.thresholds-target select')).toBeVisible();
  // A tab that names nothing has no picker.
  await tabs.getByRole('tab', { name: 'All nodes', exact: true }).click();
  await expect(page).toHaveURL(/scope_level=global/);
  await expect(page.locator('.thresholds-target')).toHaveCount(0);
});

test('Check a node opens a picker, and All rules goes back to the list', async ({ page }) => {
  await page.goto('/alerts/rules');
  await page.getByRole('button', { name: 'Check a node' }).click({ timeout: 15_000 });
  await expect(page.locator('.thresholds-check-head')).toBeVisible();
  await expect(page.getByRole('tablist')).toHaveCount(0);
  await page.getByRole('button', { name: 'All rules' }).click();
  await expect(page.getByRole('tablist')).toBeVisible();
});
