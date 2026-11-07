// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Config (ADR-210 stages 2 and 3, ADR-240).
//
// WHY A BROWSER. What the switch sends and that it then asks for a fetch, which fix the status card
// offers and what it sends, which logins the folded settings offer, which version "Compare" pairs
// with which, and the dot on the tab, are wired in `ConfigBackupTab.tsx` and `NodeDetail.tsx`, which
// Vitest never runs; the route walk opens the Overview tab and presses nothing. Which card a view
// becomes is `configBackupForm.test.ts`'s.

import type { components } from '../../src/api/schema';
import type { Page } from '@playwright/test';
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
  stale: false,
  input: 'ssh_then_telnet',
  protocol: 'ssh',
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
    { at: '2026-10-22T10:00:00Z', credential_id: LOGIN, outcome: 'auth_failed', new_version: null, input: 'ssh' },
  ],
} satisfies Schemas['ConfigBackupView'];

// A login that works, and then the same node with a fetch out (ADR-210 stage 5 decisions 54-55).
// The success is "now" so the card does not read it as two days old.
const working = {
  ...refused,
  login_confirmed: true,
  try_after: null,
  // Found over telnet: SSH did not connect (ADR-242).
  protocol: 'telnet',
  attempts: [
    { at: new Date().toISOString(), credential_id: LOGIN, outcome: 'success', new_version: true, input: 'telnet' },
  ],
} satisfies Schemas['ConfigBackupView'];
const fetching = { ...working, pending: true } satisfies Schemas['ConfigBackupView'];
const saved = {
  ...working,
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

const isPut = (path: string) => (r: { method(): string; url(): string }) =>
  r.method() === 'PUT' && new URL(r.url()).pathname.endsWith(path);
const isPost = (path: string) => (r: { method(): string; url(): string }) =>
  r.method() === 'POST' && new URL(r.url()).pathname.endsWith(path);

/** The attention dot on the Config tab alone; the mock node's other tabs may wear their own. */
const configTabDot = (page: Page) => page.getByRole('tab', { name: /^Config/ }).locator('.nd-tab-warn');

test.describe('a node not backed up yet', () => {
  test.use({ mockConfig: { overrides: overrides(off) } });

  test('the switch turns it on as it is and asks for the first fetch', async ({ page, errors }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    await expect(page.locator('.cfgb-card-off .cfgb-state')).toHaveText('Not backed up');
    // The settings wait for it to be on; only SSH only is offered before (the next test).
    await expect(page.locator('.cfgb-settings')).toHaveCount(0);
    const put = page.waitForRequest(isPut('/config-backup'));
    const fetched = page.waitForRequest(isPost('/config-backup/fetch'));
    // `click`, not `check`: the switch shows what the server stored, and this mock keeps answering
    // "off", so the box is not left ticked; what matters is what the press sent.
    await page.getByRole('switch', { name: 'Backup' }).click();
    expect((await put).postDataJSON()).toEqual({
      enabled: true,
      login_order: null,
      input: 'ssh_then_telnet',
      model_override: null,
      scan_mode: 'normal',
    });
    await fetched;
    expect(errors.uncaught).toEqual([]);
  });

  test('SSH only can be ticked before switching on, and the switch-on carries it', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    // The first fetch runs as soon as it is on, so the choice has to be offered before it (ADR-242).
    const sshOnly = page.locator('.cfgb-card').getByRole('checkbox', { name: 'SSH only' });
    await expect(sshOnly).not.toBeChecked();
    await sshOnly.check();
    const put = page.waitForRequest(isPut('/config-backup'));
    await page.getByRole('switch', { name: 'Backup' }).click();
    expect((await put).postDataJSON()).toMatchObject({ enabled: true, input: 'ssh' });
  });
});

test.describe('a node still looking for its login', () => {
  test.use({ mockConfig: { overrides: overrides(refused) } });

  test('the card says it is looking and when, and the history names the refused login', async ({
    page,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const card = page.locator('.cfgb-card-busy');
    await expect(card.locator('.cfgb-state')).toHaveText('Looking for a login that works');
    await expect(card.locator('.cfgb-facts')).toContainText('next try at');
    await expect(page.locator('.cfgb-h')).toContainText('ymock-ops-login was refused');
    await expect(page.locator('.cfgb-h .cfgb-h-proto').first()).toHaveText('SSH');
    await expect(page.getByRole('switch', { name: 'Backup' })).toBeChecked();
    // Looking is not a problem: no dot on the tab.
    await expect(configTabDot(page)).toHaveCount(0);
  });

  test('a version opens as text under its row, and Compare pairs it with the one before', async ({
    page,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const compares = page.getByRole('button', { name: 'Compare' });
    // The oldest version has nothing before it.
    await expect(compares).toHaveCount(1);

    await page.getByRole('button', { name: 'View' }).first().click();
    await expect(page.locator('.cfgb-h-viewer .cfgb-text')).toContainText('ntp server 192.0.2.1');

    const diff = page.waitForRequest((r) => new URL(r.url()).pathname.endsWith('/config-backup/diff'));
    await compares.click();
    const url = new URL((await diff).url());
    expect([url.searchParams.get('from'), url.searchParams.get('to')]).toEqual([OLD, NEW]);
    await expect(page.locator('.cfgb-h-viewer .cfgb-line-removed')).toContainText('192.0.2.9');
  });
});

test.describe('a node whose login works', () => {
  test.use({ mockConfig: { overrides: overrides(working) } });

  test('the settings fold into one line naming the login, and open to the order', async ({
    page,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    await expect(page.locator('.cfgb-card-ok .cfgb-state')).toHaveText('Backed up');
    // The protocol the login works over is one of the card's facts (ADR-242).
    await expect(page.locator('.cfgb-card-ok .cfgb-facts')).toContainText('Telnet');
    const summary = page.locator('.cfgb-settings summary');
    await expect(summary).toContainText('ymock-ops-login');
    await expect(page.locator('.cred-picker')).toBeHidden();

    await summary.click();
    await expect(page.locator('.cred-picker')).toContainText('ymock-second-login');
    await expect(page.locator('.cred-picker')).not.toContainText('ymock-community');
    await expect(page.getByRole('link', { name: 'Add a CLI login' })).toHaveAttribute(
      'href',
      '/nodes/credentials?tab=cli',
    );

    const rescan = page.waitForRequest(isPost('/config-backup/rescan'));
    await page.getByRole('button', { name: 'Search logins again' }).click();
    await rescan;
  });

  test('Save sends the order and "SSH only" as edited', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    await page.locator('.cfgb-settings summary').click();
    await page.locator('.cred-picker').getByRole('checkbox', { name: /ymock-second-login/ }).uncheck();
    await page.locator('.cfgb-settings').getByRole('checkbox', { name: 'SSH only' }).check();
    await page.locator('#cfgb-speed').selectOption('rapid');
    const put = page.waitForRequest(isPut('/config-backup'));
    await page.locator('.cfgb-settings').getByRole('button', { name: 'Save', exact: true }).click();
    expect((await put).postDataJSON()).toEqual({
      enabled: true,
      login_order: [LOGIN],
      input: 'ssh',
      model_override: null,
      scan_mode: 'rapid',
    });
  });
});

test.describe('a node with a fetch out', () => {
  test.use({ mockConfig: { overrides: overrides(fetching) } });

  test('offers Cancel fetch instead of Fetch now, and cancelling posts it', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    await expect(page.getByRole('button', { name: 'Fetch now' })).toHaveCount(0);
    const cancel = page.waitForRequest(isPost('/config-backup/cancel'));
    await page.getByRole('button', { name: 'Cancel fetch' }).click();
    await cancel;
  });
});

// The problems the card offers a fix for (ADR-240 decision 2).
const noModel = {
  ...off,
  enabled: true,
  readiness: 'no_model',
  profile_model: null,
} satisfies Schemas['ConfigBackupView'];
const everyLoginRefused = {
  ...refused,
  exhausted: true,
  try_after: null,
} satisfies Schemas['ConfigBackupView'];

test.describe('a node whose profile names no model', () => {
  test.use({ mockConfig: { overrides: overrides(noModel) } });

  test('the card asks for the model and saves it with the first fetch', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const card = page.locator('.cfgb-card-fix');
    await expect(card.locator('.cfgb-state')).toContainText('No model');
    await expect(configTabDot(page)).toHaveCount(1);
    const go = card.getByRole('button', { name: 'Save and fetch' });
    await expect(go).toBeDisabled();
    await card.locator('#cfgb-fix-model').selectOption('nxos');
    const put = page.waitForRequest(isPut('/config-backup'));
    const fetched = page.waitForRequest(isPost('/config-backup/fetch'));
    await go.click();
    expect((await put).postDataJSON()).toMatchObject({ enabled: true, model_override: 'nxos' });
    await fetched;
  });
});

test.describe('a node no login gets into', () => {
  test.use({ mockConfig: { overrides: overrides(everyLoginRefused) } });

  test('the card offers a new login and another try', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const card = page.locator('.cfgb-card-fix');
    await expect(card.locator('.cfgb-state')).toHaveText('No login worked.');
    await expect(card.getByRole('link', { name: 'Add a CLI login' })).toHaveAttribute(
      'href',
      '/nodes/credentials?tab=cli',
    );
    const rescan = page.waitForRequest(isPost('/config-backup/rescan'));
    await card.getByRole('button', { name: 'Try again' }).click();
    await rescan;
    await page.screenshot({ path: 'test-results/config-card-fix.png', fullPage: true });
  });
});

test.describe('a node Yagra has saved (ADR-222)', () => {
  test.use({ mockConfig: { overrides: overrides(saved) } });

  test('the save is a row of the history, with the startup it replaced', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const history = page.locator('.cfgb-h');
    await expect(history).toContainText('Save to startup (A person): Saved');
    await expect(history.getByRole('button', { name: 'Replaced startup' })).toHaveCount(1);
    // Nothing unsaved, so nothing offers to save.
    await expect(page.getByRole('button', { name: 'Save to startup' })).toHaveCount(0);
    await page.screenshot({ path: 'test-results/config-history.png', fullPage: true });
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
    await expect(configTabDot(page)).toHaveCount(1);
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

  test('saves to startup from the line, only after the confirmation', async ({ page }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=config`);
    const posted = page.waitForRequest(isPost('/config-backup/save'));
    await page.locator('.cfgb-unsaved').getByRole('button', { name: 'Save to startup' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toContainText('Yagra keeps a copy');
    await dialog.getByRole('button', { name: 'Save', exact: true }).click();
    await posted;
  });
});
