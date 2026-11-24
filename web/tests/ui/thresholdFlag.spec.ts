// SPDX-License-Identifier: AGPL-3.0-only
// The threshold rule dialog names its severity columns, and a 0/1 metric's rule is a severity
// rather than a number (ADR-264; Tier1).
//
// A placeholder named the column only while its box was empty, so a filled `0.5` said nothing about
// being critical. And `snmp_up` asked for a bound where `1` fires on the healthy value.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

function rule(id: string, metric: string, bounds: Record<string, number | null>): Json {
  const [template] = (defaultBodyFor('/api/v1/thresholds') as Record<string, Json>).items as Record<
    string,
    Json
  >[];
  return {
    ...template,
    id,
    scope_level: 'global',
    scope_ids: [],
    metric,
    row_match: null,
    warning_below: null,
    critical_below: null,
    warning_above: null,
    critical_above: null,
    ...bounds,
    dwell_samples: 2,
  } as Json;
}

const RULES = [
  rule('00000000-0000-4000-8000-00000000f001', 'snmp_up', { critical_below: 0.5 }),
  rule('00000000-0000-4000-8000-00000000f002', 'cisco_cpu_5min', {
    warning_above: 80,
    critical_above: 95,
  }),
];

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/thresholds': () => {
        const body = defaultBodyFor('/api/v1/thresholds') as Record<string, Json>;
        return { ...body, items: RULES, total: RULES.length } as Json;
      },
    },
  },
});

type Page = import('@playwright/test').Page;

async function openRule(page: Page, metric: string) {
  await page.goto('/alerts/rules');
  const row = page.locator('.dt-row', { hasText: metric }).first();
  await expect(row).toBeVisible({ timeout: 15_000 });
  await row.hover();
  await row.getByRole('button', { name: 'Edit' }).click();
  return page.getByRole('dialog');
}

test('a 0/1 rule reads as its alert and severity, in the list and in the dialog', async ({
  page,
}) => {
  await page.goto('/alerts/rules');
  const row = page.locator('.dt-row', { hasText: 'snmp_up' }).first();
  await expect(row.locator('.thresholds-bounds .badge')).toHaveText('C SNMP not responding', {
    timeout: 15_000,
  });

  const dialog = await openRule(page, 'snmp_up');
  await expect(dialog.getByText('SNMP not responding')).toBeVisible();
  const severity = dialog.getByRole('group', { name: 'Severity' });
  await expect(severity.getByRole('button', { name: 'Critical' })).toHaveAttribute(
    'aria-pressed',
    'true',
  );
  // No number box to mistype: the bound is what the choice stores.
  await expect(dialog.getByRole('textbox', { name: /Alert at or/ })).toHaveCount(0);
  await page.screenshot({ path: 'test-results/adr264-flag-dialog.png' });
});

test('a numeric rule names both columns, also once the boxes are filled', async ({ page }) => {
  const dialog = await openRule(page, 'cisco_cpu_5min');
  await expect(dialog.getByRole('textbox', { name: 'Alert at or above: Warning' })).toHaveValue(
    '80',
  );
  await expect(dialog.getByRole('textbox', { name: 'Alert at or above: Critical' })).toHaveValue(
    '95',
  );
  const heads = dialog.locator('.thresholds-bound-row[aria-hidden="true"] .badge');
  await expect(heads).toHaveText(['Warning', 'Critical']);
  // The headings sit over their columns: same left edge as the boxes below them.
  const headX = await heads.evaluateAll((els) => els.map((e) => e.getBoundingClientRect().left));
  const boxX = await dialog
    .getByRole('textbox', { name: /Alert at or above/ })
    .evaluateAll((els) => els.map((e) => e.getBoundingClientRect().left));
  for (let i = 0; i < 2; i++) expect(Math.abs(headX[i] - boxX[i])).toBeLessThan(2);
  await page.screenshot({ path: 'test-results/adr264-number-dialog.png' });
});
