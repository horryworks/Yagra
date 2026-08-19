// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Metric alert rules — the two things ADR-075 増分 2 changed, checked against rows the
// test controls (ADR-052 Tier1).
//
// Both assertions are about a *rendering decision derived from the row*, which is the shape Tier1
// exists for and the shape a unit test cannot reach here (Vitest does not execute a `.tsx`):
//
//   - The scope level and the scope id were one cell. Splitting them is only done if each column
//     shows its own value under its own heading — a split that left both values in one cell would
//     still compile, still pass every unit test, and look almost right.
//   - Editing the reachability rule must not offer bounds, because the engine reads none from that
//     row. "The dialog opened" is not the property; "the dialog opened *without* two inputs" is,
//     and it is decided from the rule being edited.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const PROFILE_RULE_METRIC = 'icmp_rtt_ms';

/** Two rules built from the generated `StoredThreshold`, so a change to the Rust shape reaches
 *  this fixture too: one fleet-wide reachability rule (no scope id, no bounds) and one
 *  profile-scoped rule pointed at a profile the generated `/api/v1/profiles` actually contains, so
 *  the Scope id column has a name to resolve rather than falling back to a raw id. */
function ruleset(): Json {
  const page = defaultBodyFor('/api/v1/thresholds') as Record<string, Json>;
  const [template] = page.items as Record<string, Json>[];
  const [profile] = defaultBodyFor('/api/v1/profiles') as Record<string, Json>[];
  const items = [
    {
      ...template,
      id: '00000000-0000-4000-8000-00000000f001',
      scope_level: 'global',
      scope_id: '',
      metric: '__liveness__',
      direction: 'below',
      warning: null,
      critical: null,
      dwell_samples: 3,
    },
    {
      ...template,
      id: '00000000-0000-4000-8000-00000000f002',
      scope_level: 'profile',
      scope_id: profile.id,
      metric: PROFILE_RULE_METRIC,
      direction: 'above',
      warning: 100,
      critical: 250,
      dwell_samples: 4,
    },
  ];
  return { items, total: items.length, truncated: false } as unknown as Json;
}

test.use({
  mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/thresholds': ruleset() } },
});

test('the scope level and the scope id are two columns, each carrying its own value', async ({
  page,
}) => {
  await page.goto('/alerts/rules');
  const headers = page.locator('.dt-head .dt-h');
  await expect(headers.filter({ hasText: 'Scope level' })).toHaveCount(1);
  await expect(headers.filter({ hasText: 'Scope id' })).toHaveCount(1);

  // Column order is what makes the cells addressable by index, and it is also the thing that
  // would silently change if someone reordered the array.
  const texts = await headers.allInnerTexts();
  expect(texts.indexOf('Scope id')).toBe(texts.indexOf('Scope level') + 1);
  const level = texts.indexOf('Scope level');
  const scopeId = texts.indexOf('Scope id');

  const globalRow = page.locator('.dt-row').filter({ hasText: 'Reachability' }).first();
  await expect(globalRow.locator('.dt-cell').nth(level)).toContainText('every node');
  // The whole point of the split: the badge is alone in its cell. Before this change the resolved
  // scope name sat beside it, which is what made "which of these two is the scope id?" a question
  // an operator had to ask.
  await expect(globalRow.locator('.dt-cell').nth(level)).not.toContainText('—');
  // A fleet-wide rule has no id, and the cell says so rather than showing an empty box.
  await expect(globalRow.locator('.dt-cell').nth(scopeId)).toHaveText('—');

  const profileRow = page.locator('.dt-row').filter({ hasText: PROFILE_RULE_METRIC }).first();
  await expect(profileRow.locator('.dt-cell').nth(level)).toContainText('profile');
  // Resolved to the profile's name, never the raw UUID (ui-conventions "No raw UUIDs in tables").
  const target = profileRow.locator('.dt-cell').nth(scopeId);
  await expect(target).not.toHaveText('—');
  await expect(target.locator('.yt-entity-name')).toHaveCount(1);
});

test('a rule opens for editing with its own values, and reachability offers no bounds', async ({
  page,
}) => {
  await page.goto('/alerts/rules');
  const profileRow = page.locator('.dt-row').filter({ hasText: PROFILE_RULE_METRIC }).first();
  await profileRow.getByRole('button', { name: 'Edit' }).click();

  const dialog = page.getByRole('dialog');
  await expect(dialog).toContainText('Edit threshold rule');
  // Prefilled from the row, not from the add form's defaults. `warn`/`crit`/`breaches` are the
  // three compact boxes in that order; asserting the values is what proves the round trip is
  // wired, since an empty dialog would also "open".
  const nums = dialog.locator('.thresholds-num');
  await expect(nums).toHaveCount(3);
  await expect(nums.nth(0)).toHaveValue('100');
  await expect(nums.nth(1)).toHaveValue('250');
  await expect(nums.nth(2)).toHaveValue('4');
  await dialog.getByRole('button', { name: 'Cancel' }).click();

  const globalRow = page.locator('.dt-row').filter({ hasText: 'Reachability' }).first();
  await globalRow.getByRole('button', { name: 'Edit' }).click();
  const liveness = page.getByRole('dialog');
  // The engine reads no bound off this rule, so only the breach count is offered — and the
  // engine's internal sentinel is never shown, which is the one string this screen hides.
  await expect(liveness.locator('.thresholds-num')).toHaveCount(1);
  await expect(liveness.locator('.thresholds-num')).toHaveValue('3');
  await expect(liveness).toContainText('Reachability');
  await expect(liveness).not.toContainText('__liveness__');
});
