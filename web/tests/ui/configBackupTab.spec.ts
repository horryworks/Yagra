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
  save_supported: true,
  saves: [],
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

// A login that works, and then the same node with a fetch out (ADR-210 stage 5 decisions 54-55).
const working = {
  ...refused,
  login_confirmed: true,
  try_after: null,
  attempts: [
    { at: '2026-10-22T10:00:00Z', credential_id: LOGIN, outcome: 'success', new_version: true },
  ],
} satisfies Schemas['ConfigBackupView'];
const fetching = { ...working, pending: true } satisfies Schemas['ConfigBackupView'];
const saved = {
  ...working,
  readiness: 'ready',
  enabled: true,
  saves: [
    {
      id: '00000000-0000-4000-8000-0000000000d1',
      origin: 'person',
      state: 'saved',
      requested_at: '2026-10-31T10:00:00Z',
      finished_at: '2026-10-31T10:01:00Z',
      kept_startup: true,
    },
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

test.describe('a node whose login works', () => {
  test.use({ mockConfig: { overrides: overrides(working) } });

  test('names the working login, folds the try order, and searches again on request', async ({
    page,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const head = page.locator('.cfgb-logins-head').first();
    await expect(head).toContainText('ymock-ops-login');
    await expect(page.locator('.cred-picker')).toHaveCount(0);

    await page.getByRole('button', { name: 'Change the try order' }).click();
    await expect(page.locator('.cred-picker')).toContainText('ymock-second-login');
    await expect(page.getByRole('link', { name: 'Add a CLI login' })).toHaveAttribute(
      'href',
      '/nodes/credentials?tab=cli',
    );

    const rescan = page.waitForRequest(
      (r) => r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/config-backup/rescan'),
    );
    await page.getByRole('button', { name: 'Search logins again' }).click();
    await rescan;
  });
});

test.describe('a node with a fetch out', () => {
  test.use({ mockConfig: { overrides: overrides(fetching) } });

  test('offers Cancel fetch instead of Fetch now, and cancelling posts it', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    await expect(page.getByRole('button', { name: 'Fetch now' })).toHaveCount(0);
    const cancel = page.waitForRequest(
      (r) => r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/config-backup/cancel'),
    );
    await page.getByRole('button', { name: 'Cancel fetch' }).click();
    await cancel;
  });
});

test.describe('a node Yagra can save (ADR-222)', () => {
  test.use({ mockConfig: { overrides: overrides(saved) } });

  test('lists the save and asks for another only after the confirmation', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    await expect(page.locator('.cfgb')).toContainText('Saved');
    await expect(page.getByRole('button', { name: 'Replaced startup' })).toHaveCount(1);
    const posted = page.waitForRequest(
      (r) => r.method() === 'POST' && new URL(r.url()).pathname.endsWith('/config-backup/save'),
    );
    await page.getByRole('button', { name: 'Save to startup' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toContainText('Yagra keeps a copy');
    await dialog.getByRole('button', { name: 'Save', exact: true }).click();
    await posted;
  });

  // Each of the three records says what it is and how many rows it holds, in the order they are
  // looked for (ADR-222 Inc.9 decision 6).
  test('names the three records, versions first and the fetch log last', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const titles = page.locator('.cfgb-record .nd-section-t');
    await expect(titles).toHaveText(['Stored versions 2', 'Saves to startup 1', 'Fetch log 1']);
    await page.screenshot({ path: 'test-results/config-records.png', fullPage: true });
  });
});

// A VRP device the save script read as unsaved, with what `compare configuration` printed
// (ADR-222 Inc.9).
const unsaved = {
  ...working,
  unsaved: { since: '2026-11-01T02:06:47Z', checked_at: '2026-11-04T14:06:06Z', has_output: true },
} satisfies Schemas['ConfigBackupView'];
const COMPARE =
  ' ====== Current configuration line 12 ======\nvlan batch 500 905 1000\n\n' +
  ' ====== Configuration file line 12 ======\nvlan batch 500 1000';

test.describe('a node read as unsaved (ADR-222 Inc.9)', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...overrides(unsaved),
        '/api/v1/nodes/{node_id}/config-backup/unsaved': {
          since: '2026-11-01T02:06:47Z',
          checked_at: '2026-11-04T14:06:06Z',
          text: COMPARE,
        } as unknown as Json,
      },
    },
  });

  test('opens the two sides under the line, the changed row marked', async ({ page, errors }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const line = page.locator('.cfgb-unsaved');
    await expect(line).toContainText('Unsaved since');
    await page.getByRole('button', { name: 'View differences' }).click();
    const panel = page.locator('.cfgb-udiff');
    await expect(panel).toContainText('Running · from line 12');
    await expect(panel).toContainText('Saved file · from line 12');
    // One changed row, the running side on the left, the saved side on the right.
    const changed = panel.locator('.cfgb-udiff-differs');
    await expect(changed).toHaveCount(1);
    await expect(changed.locator('[role="cell"]').first()).toHaveText('vlan batch 500 905 1000');
    await expect(changed.locator('[role="cell"]').last()).toHaveText('vlan batch 500 1000');
    // It opens directly under the line, above the settings, not at the bottom of the tab.
    const lineBox = await line.boundingBox();
    const panelBox = await panel.boundingBox();
    const settingsBox = await page.locator('.cfgb-settings').boundingBox();
    expect(lineBox && panelBox && settingsBox).toBeTruthy();
    if (lineBox && panelBox && settingsBox) {
      expect(panelBox.y).toBeGreaterThan(lineBox.y);
      expect(panelBox.y + panelBox.height).toBeLessThanOrEqual(settingsBox.y);
    }
    await page.screenshot({ path: 'test-results/config-unsaved.png' });
    await page.getByRole('button', { name: 'Hide differences' }).click();
    await expect(panel).toHaveCount(0);
    expect(errors.uncaught).toEqual([]);
  });
});
