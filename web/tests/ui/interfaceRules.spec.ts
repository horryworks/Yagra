// SPDX-License-Identifier: AGPL-3.0-only
// The port's alert-rule dialog and the rules screen's narrowed default (ADR-076 増分 5, Tier1).
//
// Three things here that no unit test in this repo can reach, because Vitest never executes a
// `.tsx` and has no layout engine:
//
//   - the dock's bell opens the **list** of rules that govern the port, and the inherited ones are
//     shown without edit controls. `portRuleForm.test.ts` proves the translation; only a browser
//     can prove the translation is wired to a dialog and that the read-only half really is
//     read-only;
//   - the form asks what to watch and how to measure it, and **never** shows a metric picker. That
//     is the whole of the user-reported complaint, and it is a rendering decision;
//   - Alerts ▸ Metric alert rules now leaves port rules out by default. The count line is the only
//     thing that makes that honest, so a missing line reads as "those rules do not exist" —
//     `thresholdQuery.test.ts` pins the request, this pins that the operator is told.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';
const IFINDEX = 1;

/** Only a `device` shows the Interfaces tab, and the dock only exists once a port is selected. */
function deviceNode(): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as { kind: string };
  body.kind = 'device';
  return body as unknown as Json;
}

/** One rule on this port and one inherited from the node, both built from the generated shape so a
 *  change to the Rust type reaches this fixture. The inherited one is deliberately a *different*
 *  metric: two rules on one metric would make "in force" ambiguous, and the property under test is
 *  that the port's own rule is editable and the node's is not. */
function portRules(): Json {
  const template = (defaultBodyFor('/api/v1/thresholds') as Record<string, Json>)
    .items as Record<string, Json>[];
  const [rule] = template;
  return [
    {
      rule: {
        ...rule,
        id: '00000000-0000-4000-8000-00000000e001',
        scope_level: 'interface',
        scope_ids: [`${NODE_ID}:${IFINDEX}`],
        metric: 'if_in_util_pct',
        direction: 'above',
        warning: null,
        critical: 90,
        dwell_samples: 3,
      },
      in_force: true,
    },
    {
      rule: {
        ...rule,
        id: '00000000-0000-4000-8000-00000000e002',
        scope_level: 'node',
        scope_ids: [NODE_ID],
        metric: 'if_out_util_pct',
        direction: 'above',
        warning: null,
        critical: 80,
        dwell_samples: 3,
      },
      in_force: true,
    },
  ] as unknown as Json;
}

test.use({
  viewport: { width: 1280, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(),
      '/api/v1/nodes/{node_id}/interfaces/{ifindex}/thresholds': () => portRules(),
    },
  },
});

async function openRules(page: import('@playwright/test').Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await page.locator('.nd-if-row').first().click();
  await expect(page.locator('.nd-if-dock')).toBeVisible({ timeout: 15_000 });
  await page.locator('.nd-if-dock-rule').click();
  await expect(page.getByRole('dialog')).toBeVisible({ timeout: 15_000 });
}

test('the port button carries the rule count and opens the rules that govern the port', async ({
  page,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await page.locator('.nd-if-row').first().click();
  await expect(page.locator('.nd-if-dock')).toBeVisible({ timeout: 15_000 });

  // The count is the discoverability half: without it an operator has to click to learn that
  // anything is watching this port at all (ADR-055 R6).
  await expect(page.locator('.nd-if-dock-rule-count')).toHaveText('2');

  await page.locator('.nd-if-dock-rule').click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();

  // Both sections render, and each holds the rule that belongs in it.
  // Addressed structurally. A `hasText` filter was the first attempt and it matched *both*
  // sections — the inherited section's hint sentence also says "this port" — so the assertions
  // below were inspecting two rows while claiming to inspect one.
  const own = dialog.locator('.ifrules-section[data-kind="own"]');
  const inherited = dialog.locator('.ifrules-section[data-kind="inherited"]');
  await expect(own).toContainText('This port');
  await expect(inherited).toContainText('Also applies');
  await expect(own.locator('.ifrules-row')).toHaveCount(1);
  await expect(inherited.locator('.ifrules-row')).toHaveCount(1);

  // Said in the port's words, not as a metric name — the whole point of the subject list.
  await expect(own.locator('.ifrules-subject')).toHaveText('Inbound traffic');
  await expect(inherited.locator('.ifrules-subject')).toHaveText('Outbound traffic');

  // 90% of the port's own speed, spelled out in bits/sec beside it. Which speed the mock reports
  // is not this test's business — that the conversion is *offered* is.
  await expect(own.locator('.ifrules-bound')).toContainText('90%');

  // 🚨 The read-only half. A row an operator cannot act on must not draw controls they can press:
  // the inherited rule is edited on the rules screen, and the dialog says so.
  await expect(own.getByRole('button', { name: 'Edit' })).toHaveCount(1);
  await expect(inherited.getByRole('button', { name: 'Edit' })).toHaveCount(0);
  await expect(inherited.getByRole('button', { name: 'Delete' })).toHaveCount(0);
  await expect(inherited).toContainText('Metric alert rules');
});

test('adding a rule asks what to watch, never for a metric name', async ({ page }) => {
  await openRules(page);
  await page.getByRole('button', { name: 'Add rule' }).click();
  const dialog = page.getByRole('dialog');

  // The complaint this increment came from: the generic dialog offered 88 metrics, of which 7 can
  // carry a rule on a port. There is no metric control here at all.
  await expect(dialog.locator('.metricpick-trigger')).toHaveCount(0);
  await expect(dialog).not.toContainText('Scope type');

  const subject = dialog.locator('.modal-field').filter({ hasText: 'What to watch' }).locator('select');
  await expect(subject).toHaveValue('in_traffic');
  // Both directions are one control away from each other — the second complaint.
  await expect(subject.locator('option')).toHaveCount(5);

  // The basis is a real choice, and picking absolute swaps the unit control for a rate unit.
  await expect(dialog.locator('.ifrules-basis input')).toHaveCount(2);
  await expect(dialog.locator('.ifrules-unit').first()).toHaveText('%');
  await dialog.locator('.ifrules-basis input').nth(1).check();
  await expect(dialog.locator('.ifrules-bounds select')).toHaveCount(1);

  // Link state has no bounds to type, and says the thing an operator cannot guess: the rule that
  // looks right by hand (`below 0.5`) can never fire.
  await subject.selectOption('link_state');
  await expect(dialog.locator('.ifrules-fixed')).toContainText('not up');
  await expect(dialog).toContainText('below 0.5');
  await expect(dialog.locator('.ifrules-num')).toHaveCount(1); // the breach count only
});

test('the rules screen says how many port rules it is not showing', async ({ page }) => {
  // The default view narrows (ADR-076 決定 12). This line is the whole of what stops a hidden rule
  // reading as a rule that does not exist — and the count comes from the server, because the rows
  // on screen are both filtered and capped.
  await page.goto('/alerts/rules');
  const hidden = page.locator('.thresholds-hidden');
  await expect(hidden).toBeVisible({ timeout: 15_000 });
  await expect(hidden).toContainText('hidden');

  // 🚨 `isVisible()` returns true for an `opacity: 0` element, so read the computed value: a
  // notice nobody can see is exactly the failure this line exists to prevent.
  expect(await hidden.evaluate((e) => Number(getComputedStyle(e).opacity))).toBe(1);

  // And the way back in is one click, not a filter the operator has to know to open.
  await hidden.getByRole('button').click();
  await expect(page).toHaveURL(/scope_level=/);
  await expect(hidden).toHaveCount(0);
});
