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
const MULTI_RULE_METRIC = 'cisco_cpu_5min';

/** Three profiles, cloned from the generated one.
 *
 *  The generator emits exactly one element per array schema — enough to show that a picker is
 *  populated, not enough to show it can hold a second choice. Everything but `id` and `name`
 *  still comes from the document, so a change to the Rust shape reaches this fixture. */
function groupList(): Record<string, Json>[] {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  // All at the root: `groupOptions` walks a tree, and a clone that kept the generated
  // `parent_id` would nest under a group that is not in the list and vanish from the picker.
  return ['d4', 'e5', 'f6'].map((tag, i) => ({
    ...template,
    id: `00000000-0000-4000-8000-0000000000${tag}`,
    parent_id: null,
    name: `Test group ${i + 1}`,
  }));
}

function profileList(): Record<string, Json>[] {
  const [template] = defaultBodyFor('/api/v1/profiles') as Record<string, Json>[];
  return ['a1', 'b2', 'c3'].map((tag, i) => ({
    ...template,
    id: `00000000-0000-4000-8000-0000000000${tag}`,
    name: `Test profile ${i + 1}`,
  }));
}

/** Two rules built from the generated `StoredThreshold`, so a change to the Rust shape reaches
 *  this fixture too: one fleet-wide reachability rule (no scope id, no bounds) and one
 *  profile-scoped rule pointed at a profile the generated `/api/v1/profiles` actually contains, so
 *  the Scope id column has a name to resolve rather than falling back to a raw id. */
function ruleset(): Json {
  const page = defaultBodyFor('/api/v1/thresholds') as Record<string, Json>;
  const [template] = page.items as Record<string, Json>[];
  const profiles = profileList();
  const [profile] = profiles;
  const items = [
    {
      ...template,
      id: '00000000-0000-4000-8000-00000000f001',
      scope_level: 'global',
      scope_ids: [],
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
      scope_ids: [profile.id],
      metric: PROFILE_RULE_METRIC,
      direction: 'above',
      warning: 100,
      critical: 250,
      dwell_samples: 4,
    },
    // ADR-078: a rule naming two profiles. Tier1 is the only place that can see what the cell
    // does with a set — a unit test reaches the request, never the rendering.
    {
      ...template,
      id: '00000000-0000-4000-8000-00000000f003',
      scope_level: 'profile',
      scope_ids: profiles.slice(0, 3).map((p) => p.id),
      metric: MULTI_RULE_METRIC,
      direction: 'above',
      warning: 80,
      critical: 90,
      dwell_samples: 3,
    },
  ];
  return { items, total: items.length, truncated: false } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/thresholds': ruleset(),
      '/api/v1/profiles': profileList() as unknown as Json,
      '/api/v1/node-groups': groupList() as unknown as Json,
    },
  },
});

test('the scope level and the scope id are two columns, each carrying its own value', async ({
  page,
}) => {
  await page.goto('/alerts/rules');
  const headers = page.locator('.dt-head .dt-h');
  await expect(headers.filter({ hasText: 'Scope type' })).toHaveCount(1);
  // Exact: 'Scope' is a prefix of 'Scope type', so a substring filter matches both columns and
  // would report success about the wrong one.
  await expect(headers.filter({ hasText: /^Scope$/ })).toHaveCount(1);

  // Column order is what makes the cells addressable by index, and it is also the thing that
  // would silently change if someone reordered the array.
  const texts = await headers.allInnerTexts();
  expect(texts.indexOf('Scope')).toBe(texts.indexOf('Scope type') + 1);
  const level = texts.indexOf('Scope type');
  const scopeId = texts.indexOf('Scope');

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

  // ADR-078: a rule naming three profiles draws two names and counts the rest. The count is the
  // half a unit test cannot reach — and drawing all three would make every row on the screen as
  // tall as the widest rule.
  const multiRow = page.locator('.dt-row').filter({ hasText: MULTI_RULE_METRIC }).first();
  const multi = multiRow.locator('.dt-cell').nth(scopeId);
  await expect(multi.locator('.yt-entity-name')).toHaveCount(2);
  await expect(multi).toContainText('and 1 more');
  // The whole list is still reachable, on the cell's title — otherwise the third target would be
  // countable and not nameable.
  const title = await multi.locator('.thresholds-scopes').getAttribute('title');
  expect(title?.split(', ')).toHaveLength(3);
});

test('the row actions are actually on screen when the row is hovered', async ({ page }) => {
  // 🚨 This is the assertion the first version of this file did NOT make, and the bug it missed
  // shipped: `.ytable-actions` is `opacity: 0` until a reveal rule fires, and every reveal rule
  // named `.ytable-row` — which a `DataTable` row is not. The icons were invisible on ten screens.
  //
  // **`isVisible()` cannot see this.** Playwright counts an `opacity: 0` element as visible (it
  // checks display, visibility and box size), so the click below succeeded the whole time and the
  // test was green while no human could find the button. Read the computed value instead.
  await page.goto('/alerts/rules');
  const row = page.locator('.dt-row').first();
  const actions = row.locator('.ytable-actions');
  const opacity = async () => Number(await actions.evaluate((e) => getComputedStyle(e).opacity));

  // Hidden until asked for — the convention these icons follow (and the half that still worked).
  expect(await opacity()).toBe(0);
  await row.hover();
  await expect.poll(opacity).toBe(1);
  // Keyboard reach is the other path, and it was broken by the same missing selector.
  await page.keyboard.press('Escape');
  await row.getByRole('button', { name: 'Edit' }).focus();
  await expect.poll(opacity).toBe(1);
});

test('the scope id is a picker, and which picker follows the level', async ({ page }) => {
  // ADR-075 増分 3. The field used to be one text box asking for a UUID that is printed nowhere in
  // the WebUI, so a profile-scoped rule could not really be created. What Tier1 can see and a unit
  // test cannot: which *control* each level renders, and whether it has anything in it — an empty
  // `<select>` is still a `<select>`, and would be exactly what a failed profile load looks like.
  const [profile] = profileList();
  await page.goto('/alerts/rules');
  await page
    .locator('.dt-row')
    .filter({ hasText: PROFILE_RULE_METRIC })
    .first()
    .getByRole('button', { name: 'Edit' })
    .click();

  const dialog = page.getByRole('dialog');
  // Addressed by an EXACT label: 'Scope' is a prefix of 'Scope type', so a substring filter
  // matches the level field too and every assertion below would be about the wrong control.
  const scope = dialog
    .locator('.modal-field')
    .filter({ has: page.locator('.modal-field-label', { hasText: /^Scope$/ }) });
  const level = dialog
    .locator('.modal-field')
    .filter({ has: page.locator('.modal-field-label', { hasText: 'Scope type' }) })
    .locator('select');

  // profile ⇒ a populated multi-select (ADR-078), showing the profile's name, with the rule's
  // own target already ticked. An empty listbox is still a listbox, which is what a failed
  // profile load looks like — so the count and the checked state are both asserted.
  await expect(scope.locator('[role="listbox"]')).toHaveCount(1);
  expect(await scope.locator('[role="option"]').count()).toBeGreaterThan(1);
  await expect(scope).toContainText(String(profile.name));
  await expect(scope.locator('[role="option"][aria-selected="true"]')).toHaveCount(1);

  // Ticking a second one ADDS rather than replaces — the whole point of the change.
  await scope.locator('[role="option"]').nth(1).click();
  await expect(scope.locator('[role="option"][aria-selected="true"]')).toHaveCount(2);

  // folder group ⇒ the inventory tree, and the hint that says a rule inherits downwards.
  await level.selectOption('group_id');
  await expect(scope.locator('[role="listbox"]')).toHaveCount(1);
  expect(await scope.locator('[role="option"]').count()).toBeGreaterThan(1);
  await expect(scope).toContainText('every group inside it');
  // Switching levels clears the targets — profile UUIDs left on a folder-group rule match nothing.
  await expect(scope.locator('[role="option"][aria-selected="true"]')).toHaveCount(0);

  // node ⇒ the typeahead, not a dropdown of the whole inventory.
  await level.selectOption('node');
  await expect(scope.locator('[role="listbox"]')).toHaveCount(0);
  await expect(scope.locator('.nodepick-trigger')).toHaveCount(1);

  // every node ⇒ no target field at all.
  await level.selectOption('global');
  await expect(
    dialog
      .locator('.modal-field')
      .filter({ has: page.locator('.modal-field-label', { hasText: /^Scope$/ }) }),
  ).toHaveCount(0);

  // The legacy tag scope is not offered for a rule being pointed somewhere new.
  const offered = await level.locator('option').evaluateAll((os) =>
    os.map((o) => (o as HTMLOptionElement).value),
  );
  expect(offered).toEqual(['global', 'profile', 'group_id', 'node']);
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

test('the metric is picked from a searchable list that explains each choice', async ({ page }) => {
  await page.goto('/alerts/rules');
  await page.getByRole('button', { name: '+ Add rule' }).click();
  const dialog = page.getByRole('dialog');

  // The field is a listbox trigger now, not a text box: the value is chosen, never typed blind.
  const trigger = dialog.locator('.metricpick-trigger');
  await expect(trigger).toHaveCount(1);
  await trigger.click();
  const list = page.locator('.metricpick-list');
  await expect(list).toBeVisible();

  // 1. A counter is not offered. `POST /thresholds` refuses one with `counter_metric`, so
  //    offering it would be offering a choice the save rejects — the error this control removes.
  //    The mock supplies `if_hc_in_octets` precisely so this can be asserted against a row that
  //    really is in the catalog rather than against an empty list.
  await expect(list.locator('.metricpick-row', { hasText: 'if_hc_in_octets' })).toHaveCount(0);
  //    …and the gauge beside it IS offered, so "nothing is offered" cannot pass this test.
  await expect(list.locator('.metricpick-row', { hasText: 'if_oper_status' })).toHaveCount(1);

  // 2. Every row carries its meaning, and the search reaches the meaning as well as the name.
  //    "port" appears in `if_oper_status`'s sentence; the row must show that sentence, not just
  //    the bare name — reading before choosing is the whole point of the control.
  const search = page.locator('.metricpick-search input');
  await search.fill('port');
  const operRow = list.locator('.metricpick-row', { hasText: 'if_oper_status' });
  await expect(operRow).toHaveCount(1);
  await expect(operRow.locator('.metricpick-meaning')).toContainText('1 = up');

  // 3. A metric nothing explains falls back to its OID rather than an empty line — the shape an
  //    operator's own collection item takes.
  await search.fill('ymock');
  const customRow = list.locator('.metricpick-row', { hasText: 'ymock_widget_temp' });
  await expect(customRow).toHaveCount(1);
  await expect(customRow.locator('.metricpick-meaning')).toContainText('1.3.6.1.4.1.99999.1.1');

  // 4. A name the catalog has never heard of can still be used — an operator may attach a
  //    collection item with any valid name, so a closed list would block a real metric.
  await search.fill('acme_widget_hz');
  await expect(list.locator('.metricpick-row', { hasText: 'if_oper_status' })).toHaveCount(0);
  const useIt = list.locator('.metricpick-row.custom');
  await expect(useIt).toContainText('acme_widget_hz');
  await useIt.click();
  await expect(dialog.locator('.metricpick-trigger')).toContainText('acme_widget_hz');

  // 5. Choosing a metric that IS explained shows the sentence beside the closed field, so the
  //    operator who never opens the list still sees what the rule watches.
  await dialog.locator('.metricpick-trigger').click();
  await page.locator('.metricpick-search input').fill('if_oper_status');
  await page.locator('.metricpick-list .metricpick-row').first().click();
  await expect(dialog.locator('.metricpick .modal-hint')).toContainText('1 = up');
});

test('Escape closes the metric list without throwing away the rule behind it', async ({ page }) => {
  // The metric picker is the first popover this product puts inside a dialog, and `AnchoredPopover`
  // and `Modal` both listen for Escape on `document` — so one press used to close both, discarding
  // a half-filled form. Two presses, two different things closing, is the property.
  await page.goto('/alerts/rules');
  await page.getByRole('button', { name: '+ Add rule' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.locator('.thresholds-num').first().fill('42');

  await dialog.locator('.metricpick-trigger').click();
  await expect(page.locator('.metricpick-list')).toBeVisible();

  await page.keyboard.press('Escape');
  await expect(page.locator('.metricpick-list')).toHaveCount(0);
  // Still open, and still holding what was typed.
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.thresholds-num').first()).toHaveValue('42');

  // …and the second press still closes the dialog, so the fix did not just break Escape.
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
});
