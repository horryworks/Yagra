// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Config (ADR-210 stage 2).
//
// WHY A BROWSER. The form and what it sends, and which version "Compare" pairs with which, are
// wired in `ConfigBackupTab.tsx`, which Vitest never runs; the route walk opens the Overview tab
// and presses nothing.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const NODE_ID = '00000000-0000-4000-8000-0000000000cb';
const LOGIN = '00000000-0000-4000-8000-0000000000c1';
const NEW = 'b'.repeat(40);
const OLD = 'a'.repeat(40);

const view = {
  available: true,
  enabled: false,
  credential_id: null,
  input: 'ssh',
  model_override: null,
  profile_model: 'ios',
  readiness: 'off',
  status: { status: 'success', last_end: '2026-10-22T09:00:00Z', observed_at: '2026-10-22T09:05:00Z' },
  models: ['ios', 'nxos'],
} satisfies Schemas['ConfigBackupView'];

const credentials = (() => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  return [
    { ...first, id: LOGIN, name: 'ymock-ops-login', kind: 'cli_login' },
    { ...first, id: '00000000-0000-4000-8000-0000000000c2', name: 'ymock-community', kind: 'snmp_v2c' },
  ] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/config-backup': view as unknown as Json,
      '/api/v1/nodes/{node_id}/config-backup/versions': [
        { oid: NEW, stored_at: '2026-10-22T09:00:00Z' },
        { oid: OLD, stored_at: '2026-10-21T09:00:00Z' },
      ] as unknown as Json,
      '/api/v1/nodes/{node_id}/config-backup/versions/{oid}': {
        oid: NEW,
        text: 'hostname ymock-sw\nntp server 192.0.2.1\n',
      } as unknown as Json,
      '/api/v1/nodes/{node_id}/config-backup/diff': {
        from: OLD,
        to: NEW,
        lines: [
          { kind: 'same', text: 'hostname ymock-sw' },
          { kind: 'removed', text: 'ntp server 192.0.2.9' },
          { kind: 'added', text: 'ntp server 192.0.2.1' },
        ],
      } as unknown as Json,
      '/api/v1/credentials': credentials,
    },
  },
});

test('switching a node on sends the CLI login and the connection', async ({ page, errors }) => {
  await page.goto(`/nodes/${NODE_ID}?tab=config`);
  // Only CLI logins are offered.
  const login = page.locator('#cfgb-login');
  await expect(login.locator('option', { hasText: 'ymock-ops-login' })).toHaveCount(1);
  await expect(login.locator('option', { hasText: 'ymock-community' })).toHaveCount(0);

  await page.getByText('Back up config').click();
  await login.selectOption(LOGIN);
  await page.locator('#cfgb-input').selectOption('telnet');
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname.endsWith('/config-backup'),
  );
  await page.getByRole('button', { name: 'Save' }).click();
  expect((await put).postDataJSON()).toEqual({
    enabled: true,
    credential_id: LOGIN,
    input: 'telnet',
    model_override: null,
  });
  expect(errors.uncaught).toEqual([]);
});

test('a version opens as text, and Compare pairs it with the one before', async ({ page }) => {
  await page.goto(`/nodes/${NODE_ID}?tab=config`);
  const compares = page.getByRole('button', { name: 'Compare' });
  // The oldest version has nothing before it.
  await expect(compares).toHaveCount(1);

  await page.getByRole('button', { name: 'View' }).first().click();
  await expect(page.locator('.cfgb-text')).toContainText('ntp server 192.0.2.1');

  const diff = page.waitForRequest((r) => new URL(r.url()).pathname.endsWith('/config-backup/diff'));
  await compares.click();
  const url = new URL((await diff).url());
  expect([url.searchParams.get('from'), url.searchParams.get('to')]).toEqual([OLD, NEW]);
  await expect(page.locator('.cfgb-line-removed')).toContainText('192.0.2.9');
});
