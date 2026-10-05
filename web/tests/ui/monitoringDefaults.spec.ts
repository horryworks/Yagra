// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Monitoring defaults after ADR-200 Inc.5: what the screen does in place of the
// sentences it lost.
//
// 🚨 The first test is the one that matters. The retention card used to say, above its rows, that
// lowering a window deletes older data on the next sweep. That sentence went only because the save
// now asks before it shortens anything — so if the dialog stops appearing, the warning is simply
// gone from the product, and the data with it.
//
// The route walk opens this screen once and never presses Save, so none of this is visible to it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const SAVED = {
  alert_linked_days: 90,
  unmatched_event_hours: 24,
  report_run_days: 90,
  flow_days: 30,
  diagnostic_days: 90,
};

/** One editable row (traffic flows, 30 days), patched onto the generated policy so the fixture
 *  follows `RetentionPolicy` rather than transcribing it (ADR-052 decision 2). */
function retention(): Json {
  const body = defaultBodyFor('/api/v1/settings/retention') as {
    settings: Record<string, unknown>;
    rows: Record<string, unknown>[];
  };
  body.settings = { ...body.settings, ...SAVED };
  body.rows = [
    {
      ...body.rows[0],
      subject: 'flow_records',
      tunable: 'settings',
      field: 'flow_days',
      unit: 'days',
    },
  ];
  return body as unknown as Json;
}

/** Every walk on except ARP, at an hour. */
function neighbors(): Json {
  const body = defaultBodyFor('/api/v1/settings/neighbors') as Record<string, unknown>;
  return {
    ...body,
    enabled: true,
    interval_secs: 3600,
    l3_enabled: true,
    l3_interval_secs: 3600,
    arp_enabled: false,
    arp_interval_secs: 3600,
    routing_enabled: true,
    routing_interval_secs: 3600,
    media_enabled: true,
    media_interval_secs: 3600,
    min_interval_secs: 300,
    max_interval_secs: 86400,
  } as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/settings/retention': () => retention(),
      '/api/v1/settings/neighbors': () => neighbors(),
    },
  },
});

const puts = (mock: { requests: { method: string; pathname: string }[] }) =>
  mock.requests.filter((r) => r.method === 'PUT' && r.pathname === '/api/v1/settings/retention');

test('shortening a retention window asks first; lengthening one does not', async ({
  page,
  mock,
}) => {
  await page.goto('/settings/system');
  const card = page.locator('.card', { has: page.locator('.card-title', { hasText: 'Data retention' }) });
  const box = card.getByRole('textbox', { name: 'Traffic flows' });
  await expect(box).toHaveValue('30');

  await box.fill('7');
  await card.getByRole('button', { name: 'Save' }).click();
  const dialog = page.locator('[aria-modal="true"]');
  await expect(dialog).toContainText('Shorter retention deletes older data');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
  expect(puts(mock), 'cancelling must not send the shorter window').toHaveLength(0);

  // Confirming sends it.
  await card.getByRole('button', { name: 'Save' }).click();
  await page.getByRole('button', { name: 'Save and delete older data' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => puts(mock).length).toBe(1);

  // A longer window deletes nothing, so it is saved without a question.
  await box.fill('60');
  await card.getByRole('button', { name: 'Save' }).click();
  await expect.poll(() => puts(mock).length).toBe(2);
  await expect(dialog).toHaveCount(0);
});

test('the ARP load warning appears only once the walk is switched on', async ({ page }) => {
  await page.goto('/settings/system');
  const warn = page.locator('.sys-setting-warn');
  await expect(page.getByRole('button', { name: 'ARP / IPv6 neighbor cache' })).toHaveCount(1);
  await expect(warn).toHaveCount(0);

  // The toggle beside the ARP name is the third walk's checkbox.
  await page.locator('.sys-setting-toggle input').nth(2).check();
  await expect(warn).toContainText('adds load on busy switches');
  await expect(warn.getByRole('link', { name: /Discovery/ })).toHaveAttribute(
    'href',
    '/nodes/discovery',
  );

  // The walk's name opens what it collects.
  await page.getByRole('button', { name: 'ARP / IPv6 neighbor cache' }).click();
  await expect(page.locator('.infotip-pop')).toContainText('only source');
});
