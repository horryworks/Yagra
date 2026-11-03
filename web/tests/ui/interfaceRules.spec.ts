// SPDX-License-Identifier: AGPL-3.0-only
// The port's thresholds dialog (ADR-076 Inc.5, redrawn by ADR-230 decision 7; Tier1).
//
// What no unit test in this repo can reach, because Vitest never executes a `.tsx` and has no
// layout engine:
//
//   - the dock's button says what it opens, in words, and counts the port's own rules;
//   - the dialog draws one row per subject, each with the one action that fits it — Edit and
//     Revert on the port's own rule, Override on an inherited one, Set on a subject nothing
//     watches. `portSubjectRows.test.ts` proves the choice; only a browser proves it is wired;
//   - Override opens the form at the inherited bounds, and the form asks what to watch, never for
//     a metric name.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';
const IFINDEX = 1;

/** One rule on this port and one inherited from the node, both built from the generated shape so a
 *  change to the Rust type reaches this fixture. Different subjects, so each row has one answer. */
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
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces/{ifindex}/thresholds': () => portRules(),
    },
  },
});

async function openDock(page: import('@playwright/test').Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await page.locator('.nd-if-row .nd-if-name').first().click();
  await expect(page.locator('.nd-if-dock')).toBeVisible({ timeout: 15_000 });
}

async function openRules(page: import('@playwright/test').Page) {
  await openDock(page);
  await page.locator('.nd-if-dock-rule').click();
  await expect(page.getByRole('dialog')).toBeVisible({ timeout: 15_000 });
}

const subjectRow = (page: import('@playwright/test').Page, subject: string) =>
  page.getByRole('dialog').locator(`.ifrules-row[data-subject="${subject}"]`);

test('the port button names what it opens and counts the port’s own rules', async ({ page }) => {
  await openDock(page);
  // Words, not a bell, and the number is the port's own rules only: the inherited one is not
  // counted, because every port has those (ADR-230).
  await expect(page.locator('.nd-if-dock-rule')).toHaveText('Thresholds · 1');
});

test('each subject row offers the one action that fits it', async ({ page }) => {
  await openRules(page);
  const dialog = page.getByRole('dialog');

  // The port's own rule: said in the port's words, edited or reverted here.
  const own = subjectRow(page, 'in_traffic');
  await expect(own.locator('.ifrules-subject')).toHaveText('Inbound traffic');
  await expect(own.locator('.ifrules-bound')).toContainText('90%');
  await expect(own.locator('.ifrules-scope')).toHaveText('This port');
  await expect(own.getByRole('button', { name: 'Edit' })).toHaveCount(1);
  await expect(own.getByRole('button', { name: 'Revert' })).toHaveCount(1);

  // Inherited from the node: overridden, never edited in place.
  const inherited = subjectRow(page, 'out_traffic');
  await expect(inherited.locator('.ifrules-scope')).toContainText('Node');
  await expect(inherited.getByRole('button', { name: 'Override' })).toHaveCount(1);
  await expect(inherited.getByRole('button', { name: 'Edit' })).toHaveCount(0);

  // Nothing watches the link state: one button to start.
  const link = subjectRow(page, 'link_state');
  await expect(link.locator('.ifrules-bound')).toHaveText('Not watched');
  await expect(link.getByRole('button', { name: 'Set' })).toHaveCount(1);

  // A node-wide metric is not one of this port's, and the way to them is the footer link.
  await expect(dialog).not.toContainText('Also applies');
  await expect(dialog.getByRole('link', { name: 'Node-wide thresholds' })).toHaveAttribute(
    'href',
    `/alerts/rules?check=${NODE_ID}`,
  );
});

test('override opens the form at the inherited bounds', async ({ page }) => {
  await openRules(page);
  await subjectRow(page, 'out_traffic').getByRole('button', { name: 'Override' }).click();
  const dialog = page.getByRole('dialog');
  const subject = dialog.locator('.modal-field').filter({ hasText: 'What to watch' }).locator('select');
  await expect(subject).toHaveValue('out_traffic');
  await expect(dialog.locator('.ifrules-num').nth(1)).toHaveValue('80');
});

test('the form asks what to watch, never for a metric name', async ({ page }) => {
  await openRules(page);
  await subjectRow(page, 'in_traffic').getByRole('button', { name: 'Edit' }).click();
  const dialog = page.getByRole('dialog');

  // The complaint ADR-076 Inc.5 came from: the generic dialog offered 88 metrics, of which 7 can
  // carry a rule on a port. There is no metric control here at all.
  await expect(dialog.locator('.metricpick-trigger')).toHaveCount(0);
  await expect(dialog).not.toContainText('Scope type');

  const subject = dialog.locator('.modal-field').filter({ hasText: 'What to watch' }).locator('select');
  await expect(subject).toHaveValue('in_traffic');
  await expect(subject.locator('option')).toHaveCount(5);

  // The basis is a real choice, and picking absolute swaps the unit control for a rate unit.
  await expect(dialog.locator('.ifrules-basis input')).toHaveCount(2);
  await expect(dialog.locator('.ifrules-unit').first()).toHaveText('%');
  await dialog.locator('.ifrules-basis input').nth(1).check();
  await expect(dialog.locator('.ifrules-bounds select')).toHaveCount(1);

  // Link state has no bounds to type, and says the thing an operator cannot guess: the rule that
  // looks right by hand (`below 0.5`) can never fire. It is behind the ⓘ beside the condition.
  await subject.selectOption('link_state');
  await expect(dialog.locator('.ifrules-fixed')).toContainText('not up');
  await dialog.locator('.ifrules-fixed .infotip-btn').click();
  await expect(page.locator('.infotip-pop')).toContainText('below 0.5');
  await page.keyboard.press('Escape');
  await expect(page.locator('.infotip-pop')).toHaveCount(0);
  await expect(dialog).toBeVisible();
  await expect(dialog.locator('.ifrules-num')).toHaveCount(1); // the breach count only
});
