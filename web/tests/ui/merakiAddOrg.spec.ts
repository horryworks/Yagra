// SPDX-License-Identifier: AGPL-3.0-only
// Adding Meraki organizations under a key that is already stored (ADR-164 Inc.6).
//
// Why Tier1: every decision the dialog makes is `merakiAddOrg.ts`, unit-tested — which field the
// request carries, which rows can be ticked, which region follows a key. What Vitest cannot run is
// the dialog handing those answers to a request and to a checkbox, and that is where the contract
// lives: the server answers a body naming both `api_key` and `credential_id` with 400.
//
// The walk cannot be the reader. It opens this screen and no dialog, and the generated credential
// is kind `ymock-kind` — not a Meraki key — so the choice this spec presses is never even drawn.
//
// It presses Find and stops: discovering persists nothing. The bodies are the generated ones,
// patched — a hand-written fixture would be a second copy of the contract (testing.md).

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const KEY_ID = '00000000-0000-4000-8000-0000000000f1';
const KEY_NAME = 'ymock-meraki-key';
const CHINA = 'https://api.meraki.cn';

/** One stored Meraki key. The generated kind is a placeholder no picker would offer. */
const credentials = (() => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  return [{ ...first, id: KEY_ID, name: KEY_NAME, kind: 'meraki_api' }] as unknown as Json;
})();

/** One organization already polled with that key, in a region that is not the dialog's default —
 *  so "the region followed the key" is a change, not the value the select started on. */
const orgs = (() => {
  const [first] = defaultBodyFor('/api/v1/meraki/orgs') as unknown as Schemas['MerakiOrgView'][];
  return [{ ...first, credential_id: KEY_ID, base_url: CHINA }] as unknown as Json;
})();

/** What the key can see: one organization monitored here already, one that is not. */
const discovered: Schemas['MerakiOrgOption'][] = [
  { id: '1001', name: 'ymock-org-here', already_added: true },
  { id: '1002', name: 'ymock-org-new', already_added: false },
];

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/credentials': credentials,
      '/api/v1/meraki/orgs': orgs,
      '/api/v1/meraki/orgs/discover': discovered as unknown as Json,
    },
  },
});

test('a saved key is sent by its id alone, and an organization already added cannot be ticked', async ({
  page,
  errors,
}) => {
  await page.goto('/settings/integrations/meraki');
  // The row names the stored key it is polled with — the name, never the id.
  await expect(page.locator('.meraki-org-key')).toHaveText(`API key: ${KEY_NAME}`);

  await page.getByRole('button', { name: '+ Add organization' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByLabel('Use a saved key').check();

  // The first stored key is preselected, and the region moved to where that key is used.
  const selects = dialog.locator('select');
  await expect(selects.nth(1)).toHaveValue(KEY_ID);
  await expect(selects.nth(0)).toHaveValue(CHINA);
  // The box to type a key into is gone, not merely emptied.
  await expect(dialog.locator('input[type="password"]')).toHaveCount(0);

  const asked = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/meraki/orgs/discover',
  );
  await dialog.getByRole('button', { name: 'Find organizations' }).click();
  const body = (await asked).postDataJSON() as Record<string, unknown>;
  expect(body).toEqual({ credential_id: KEY_ID, base_url: CHINA });
  // Said outright as well, because it is the point: the server refuses a body that names both.
  expect(Object.keys(body)).not.toContain('api_key');

  const here = dialog.locator('.meraki-check-row').filter({ hasText: 'ymock-org-here' });
  const fresh = dialog.locator('.meraki-check-row').filter({ hasText: 'ymock-org-new' });
  await expect(here.locator('input[type="checkbox"]')).toBeDisabled();
  await expect(here.locator('input[type="checkbox"]')).not.toBeChecked();
  await expect(here).toContainText('Already added');
  await expect(fresh.locator('input[type="checkbox"]')).toBeEnabled();
  await expect(fresh.locator('input[type="checkbox"]')).toBeChecked();
  await expect(fresh).not.toContainText('Already added');
  // One of the two can be added, and the button counts that one.
  await expect(dialog.getByRole('button', { name: 'Add 1 organization' })).toBeEnabled();

  expect(errors.uncaught).toEqual([]);
});
