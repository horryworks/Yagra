// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Config (ADR-210 stages 2 and 3).
//
// WHY A BROWSER. What Start sends and that it then asks for a fetch, which logins the order offers,
// the state line naming a refused login, and which version "Compare" pairs with which, are wired in
// `ConfigBackupTab.tsx`, which Vitest never runs; the route walk opens the Overview tab and presses
// nothing.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const NODE_ID = '00000000-0000-4000-8000-0000000000cb';
const LOGIN = '00000000-0000-4000-8000-0000000000c1';
const SECOND = '00000000-0000-4000-8000-0000000000c3';
const NEW = 'b'.repeat(40);
const OLD = 'a'.repeat(40);

const off = {
  available: true,
  enabled: false,
  login_order: null,
  candidates: [LOGIN, SECOND],
  credential_id: null,
  login_confirmed: false,
  pending: false,
  try_after: null,
  exhausted: false,
  attempts: [],
  input: 'ssh',
  scan_mode: 'normal',
  model_override: null,
  profile_model: 'ios',
  readiness: 'off',
  status: null,
  models: ['ios', 'nxos'],
} satisfies Schemas['ConfigBackupView'];

const refused = {
  ...off,
  enabled: true,
  readiness: 'ready',
  credential_id: LOGIN,
  try_after: '2026-10-22T10:10:00Z',
  attempts: [
    { at: '2026-10-22T10:00:00Z', credential_id: LOGIN, outcome: 'auth_failed', new_version: null },
  ],
} satisfies Schemas['ConfigBackupView'];

const credentials = (() => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  return [
    { ...first, id: LOGIN, name: 'ymock-ops-login', kind: 'cli_login' },
    { ...first, id: '00000000-0000-4000-8000-0000000000c2', name: 'ymock-community', kind: 'snmp_v2c' },
    { ...first, id: SECOND, name: 'ymock-second-login', kind: 'cli_login' },
  ] as unknown as Json;
})();

const overrides = (view: Schemas['ConfigBackupView']) => ({
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
});

test.describe('a node not backed up yet', () => {
  test.use({ mockConfig: { overrides: overrides(off) } });

  test('Start sends the logins in their order and asks for the first fetch', async ({
    page,
    errors,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    // Only CLI logins are offered, every one ticked in the try order.
    const picker = page.locator('.cred-picker');
    await expect(picker).toContainText('ymock-ops-login');
    await expect(picker).toContainText('ymock-second-login');
    await expect(picker).not.toContainText('ymock-community');

    await picker.getByRole('checkbox', { name: /ymock-second-login/ }).uncheck();
    await page.locator('#cfgb-input').selectOption('telnet');
    await page.locator('#cfgb-speed').selectOption('rapid');
    const put = page.waitForRequest(
      (r) => r.method() === 'PUT' && new URL(r.url()).pathname.endsWith('/config-backup'),
    );
    const fetched = page.waitForRequest(
      (r) => r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/config-backup/fetch'),
    );
    await page.getByRole('group', { name: 'Backup' }).getByRole('button', { name: 'On' }).click();
    expect((await put).postDataJSON()).toEqual({
      enabled: true,
      login_order: [LOGIN],
      input: 'telnet',
      model_override: null,
      scan_mode: 'rapid',
    });
    await fetched;
    expect(errors.uncaught).toEqual([]);
  });
});

test.describe('a node whose login was refused', () => {
  test.use({ mockConfig: { overrides: overrides(refused) } });

  test('the state line names the refused login and when the next one is tried', async ({
    page,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const state = page.locator('.cfgb-state');
    await expect(state).toContainText('ymock-ops-login was refused');
    await expect(state).toContainText('next try at');
    // The attempt is in the list too, with the same words.
    await expect(page.locator('.cfgb-tone-bad', { hasText: 'was refused' })).toHaveCount(2);
    // A backup that is on says so on its switch; there is no Stop beside Fetch now to misread.
    const backup = page.getByRole('group', { name: 'Backup' });
    await expect(backup.getByRole('button', { name: 'On' })).toHaveAttribute('aria-pressed', 'true');
    await expect(page.getByRole('button', { name: 'Stop' })).toHaveCount(0);
  });

  test('a version opens as text, and Compare pairs it with the one before', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const compares = page.getByRole('button', { name: 'Compare' });
    // The oldest version has nothing before it.
    await expect(compares).toHaveCount(1);

    await page.getByRole('button', { name: 'View' }).first().click();
    await expect(page.locator('.cfgb-text')).toContainText('ntp server 192.0.2.1');

    const diff = page.waitForRequest((r) =>
      new URL(r.url()).pathname.endsWith('/config-backup/diff'),
    );
    await compares.click();
    const url = new URL((await diff).url());
    expect([url.searchParams.get('from'), url.searchParams.get('to')]).toEqual([OLD, NEW]);
    await expect(page.locator('.cfgb-line-removed')).toContainText('192.0.2.9');
  });
});
