// SPDX-License-Identifier: AGPL-3.0-only
// Node detail ▸ Alert rules (ADR-229, Tier1).
//
// Why Tier1: the grouping and which rule the button edits are decided in `nodeRules.ts`, which
// Vitest runs. What only a browser proves is the wiring: the tab reads the node's route, a metric
// row shows the rule in force with where it was decided, opening the row shows the rule it beat,
// and the button opens the rule dialog at the node level for this node.

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
  await page.goto(`/nodes/${NODE_ID}?tab=rules`);
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
