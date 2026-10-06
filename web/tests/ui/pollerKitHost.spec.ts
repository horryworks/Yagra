// SPDX-License-Identifier: AGPL-3.0-only
// The kit dialog says an address is not on the bus certificate before it sends one (ADR-200 Inc.30).
//
// 🚨 The warning this pins shipped with ADR-065 and never rendered: the Remote pollers panel compared
// the certificate against a list of site addresses that was always empty, because no such list is
// stored. The only place the address a site will dial is known is the field in this dialog, so the
// comparison lives here. Without it the operator learns of the mismatch from a 400 after the click
// (`address_not_in_certificate`) — or, before the server refused, at the site.
//
// Vitest covers the judgement (`lib/busCert.ts::uncoveredKitHost`); this is the wiring: the dialog
// reads the certificate, draws the line, and refuses the button.

import type { Route } from '@playwright/test';
import { expect, test } from '../support/app';

const json = (body: unknown) => (route: Route) =>
  route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) });

const bus = {
  remote_enabled: true,
  can_switch: true,
  certificate: {
    certificate: '-----BEGIN CERTIFICATE-----\nx\n-----END CERTIFICATE-----\n',
    subject: 'CN=nats',
    issuer: 'CN=nats',
    sans: ['nats', 'localhost', '127.0.0.1', '::1', 'yagra.example.net'],
    not_before: '2026-01-01T00:00:00Z',
    not_after: '2027-02-01T00:00:00Z',
    fingerprint_sha256: 'ab'.repeat(32),
    key_algorithm: 'ECDSA P-256',
    expires_in_days: 300,
    issued_at: '2026-01-01T00:00:00Z',
    issued_by: null,
    materialized: true,
    key_unreadable: false,
  },
};

test('the kit dialog refuses an address the bus certificate does not cover', async ({ page }) => {
  await page.route('**/api/v1/settings/bus', json(bus));
  await page.goto('/settings/pollers');

  // The token column's button opens the dialog; its text is "Own token" or "Shared secret"
  // depending on the generated row, so match either.
  await page
    .getByRole('button', { name: /^(Own token|Shared secret)$/ })
    .first()
    .click();
  const modal = page.locator('.modal').last();
  await expect(modal).toBeVisible();

  const host = modal.getByLabel('Address this site will dial');

  const submit = modal.getByRole('button', { name: /Issue (a new )?token & download/ });
  await host.fill('203.0.113.10');
  await expect(modal.locator('.field-error')).toContainText('203.0.113.10');
  await expect(submit).toBeDisabled();

  // And the accepting case: a check that only ever refuses passes when the button is refused
  // unconditionally.
  await host.fill('Yagra.Example.Net');
  await expect(modal.locator('.field-error')).toHaveCount(0);
  await expect(submit).toBeEnabled();
});
