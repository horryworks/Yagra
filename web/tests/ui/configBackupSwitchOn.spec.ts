// SPDX-License-Identifier: AGPL-3.0-only
// Config backups ▸ switching several nodes on at once (ADR-224 decision 6, ADR-240 Inc.2 decision 12).
//
// WHY A BROWSER. Which logins the dialog offers, that nothing is ticked at first, that it cannot be
// sent until one is, and what the switch then carries are wired in `ConfigBackupSwitchOnModal.tsx`
// and `ConfigBackupsPage.tsx`, which Vitest never runs. How many nodes keep their own logins is
// `configBackupList.test.ts`'s.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const FRESH = '00000000-0000-4000-8000-0000000000e1';
const STOPPED = '00000000-0000-4000-8000-0000000000e2';
const LOGIN = '00000000-0000-4000-8000-0000000000c1';
const SECOND = '00000000-0000-4000-8000-0000000000c3';

const row = (node_id: string, configured: boolean) =>
  ({
    node_id,
    configured,
    enabled: false,
    login_confirmed: false,
    pending: false,
    model: 'ios',
    poller: 'ready',
  }) satisfies Schemas['ConfigBackupRow'];

const credentials = (() => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  return [
    { ...first, id: LOGIN, name: 'ymock-ops-login', kind: 'cli_login' },
    { ...first, id: '00000000-0000-4000-8000-0000000000c2', name: 'ymock-community', kind: 'snmp_v2c' },
    { ...first, id: SECOND, name: 'ymock-second-login', kind: 'cli_login' },
  ] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/config-backups': [row(FRESH, false), row(STOPPED, true)] as unknown as Json,
      '/api/v1/credentials': credentials,
    },
  },
});

test('the dialog offers the CLI logins unticked and sends the ones picked', async ({ page, errors }) => {
  await page.goto('/nodes/config-backups');
  await page.getByRole('checkbox', { name: 'Select all' }).click();
  await page.getByRole('button', { name: 'Turn on 2' }).click();

  const dialog = page.getByRole('dialog');
  const picker = dialog.locator('.cred-picker');
  await expect(picker).toContainText('ymock-second-login');
  await expect(picker).not.toContainText('ymock-community');
  await expect(picker.getByRole('checkbox', { checked: true })).toHaveCount(0);
  // The stopped node has settings of its own and keeps its logins.
  await expect(dialog.locator('.form-status')).toHaveText('1 already set up keeps its own logins');
  const submit = dialog.getByRole('button', { name: 'Turn on 2' });
  await expect(submit).toBeDisabled();

  await picker.getByRole('checkbox', { name: /ymock-second-login/ }).check();
  const sent = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/config-backups/switch',
  );
  await submit.click();
  expect((await sent).postDataJSON()).toEqual({
    node_ids: [FRESH, STOPPED],
    enabled: true,
    input: 'ssh_then_telnet',
    login_order: [SECOND],
  });
  expect(errors.uncaught).toEqual([]);
});

test('Select all in the dialog sends every CLI login as the default', async ({ page }) => {
  await page.goto('/nodes/config-backups');
  await page.getByRole('checkbox', { name: 'Select all' }).click();
  await page.getByRole('button', { name: 'Turn on 2' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('button', { name: 'Select all' }).click();
  const sent = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/config-backups/switch',
  );
  await dialog.getByRole('button', { name: 'Turn on 2' }).click();
  expect((await sent).postDataJSON()).toMatchObject({ login_order: null });
});
