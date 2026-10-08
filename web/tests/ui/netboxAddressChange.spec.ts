// SPDX-License-Identifier: AGPL-3.0-only
// Editing a NetBox deployment's address to another host (ADR-178 decision 3).
//
// Why Tier1: the decision itself (`netboxBaseUrl.ts::addressChangeNeedsToken`) and the field's
// three states (`secretField.ts`) are unit-tested. What Vitest cannot run is the focus: the token
// box opens by itself part-way through typing the new address, and on a lab box it took the focus
// there, so the rest of the address went into the token box — and, being non-empty, also enabled
// Save. Only a browser typing key by key shows that.
//
// Nothing is saved: the spec types, reads the form, and stops.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const OLD = 'https://netbox.example.com';
const NEW = 'http://192.0.2.10:8000';

const servers = (() => {
  const [first] = defaultBodyFor('/api/v1/netbox/servers') as unknown as Schemas['NetboxServerView'][];
  return [{ ...first, name: 'ymock-netbox', base_url: OLD, enabled: true }] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/netbox/servers': servers },
  },
});

test('typing a new host keeps the focus in Base URL, and Save waits for the token', async ({
  page,
  errors,
}) => {
  await page.goto('/settings/integrations/netbox');
  await page.getByRole('button', { name: 'Edit' }).first().click();
  const dialog = page.getByRole('dialog');
  const baseUrl = dialog.getByLabel('Base URL');
  const token = dialog.locator('#netbox-token');
  const save = dialog.getByRole('button', { name: 'Save' });

  // Before the change: the stored mark, no box.
  await expect(token).toHaveCount(0);
  await expect(dialog.getByRole('button', { name: 'Replace' })).toBeVisible();

  await baseUrl.fill('');
  await baseUrl.pressSequentially(NEW);

  // Every character landed in Base URL, none in the token box.
  await expect(baseUrl).toHaveValue(NEW);
  await expect(baseUrl).toBeFocused();
  await expect(token).toHaveValue('');
  await expect(dialog.getByText('The address now points at a different host.', { exact: false }))
    .toBeVisible();
  // No way back to the stored token, and no Save until a new one is typed.
  await expect(dialog.getByRole('button', { name: 'Keep stored' })).toHaveCount(0);
  await expect(save).toBeDisabled();

  await token.fill('ymock-token');
  await expect(save).toBeEnabled();

  // Back to the old host: the typed token stays visible (with Keep stored) rather than hiding
  // behind the stored mark while Save would still send it.
  await baseUrl.fill(OLD);
  await expect(token).toHaveValue('ymock-token');
  await expect(dialog.getByRole('button', { name: 'Keep stored' })).toBeVisible();

  expect(errors.uncaught).toEqual([]);
});
