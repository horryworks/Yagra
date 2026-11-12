// SPDX-License-Identifier: AGPL-3.0-only
// The order credentials are tried in, as the two screens that set it send it (ADR-205 Inc.2).
//
// WHY A BROWSER. The arithmetic is unit-tested (`lib/reorder.ts`, `credentialSelection.ts`), but
// what reaches the server is wired in two `.tsx` files Vitest never runs: which array the arrows
// rebuild, and which one the request carries. The route walk opens both screens and presses
// nothing, so neither request is ever made there.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { dragByKeys, dragByMouse, grip } from '../support/sortable';

type Schemas = components['schemas'];

const S1 = '00000000-0000-4000-8000-0000000000b1';
const H1 = '00000000-0000-4000-8000-0000000000b2';
const S2 = '00000000-0000-4000-8000-0000000000b3';

/** Two SNMP credentials with an HTTP one between them, in the order the server lists them. */
const credentials = (() => {
  const [first] = defaultBodyFor(
    '/api/v1/credentials',
  ) as unknown as Schemas['CredentialSummary'][];
  return [
    { ...first, id: S1, name: 'ymock-first', kind: 'snmp_v2c' },
    { ...first, id: H1, name: 'ymock-http', kind: 'http_auth' },
    { ...first, id: S2, name: 'ymock-second', kind: 'snmp_v3' },
  ] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/credentials': credentials,
      // The generated list holds a scan still running, which Discovery reattaches to and greys the
      // form for. No scan on record, so the form is the one a fresh visit gets.
      '/api/v1/discovery/scans': [],
    },
  },
});

test('dragging an SNMP credential up saves the whole order, the HTTP row where it stood', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/credentials');
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/credentials/order',
  );
  // With the mouse, by the grip (ADR-255): the one test that proves a pointer drag works here.
  await dragByMouse(page, page, 'ymock-second', page.locator('.dt-row', { hasText: 'ymock-first' }));
  const body = (await put).postDataJSON() as { ids: string[] };
  expect(body.ids).toEqual([S2, H1, S1]);
  expect(errors.uncaught).toEqual([]);
});

// The grips work again as soon as a save returns, before the re-read that shows the new order. A
// second drop in that gap must build on the order just saved, not on the list the server last sent.
test('a second drag before the list is re-read moves on from the order just saved', async ({
  page,
}) => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  const three = ['a', 'b', 'c'].map((n, i) => ({
    ...first,
    id: `00000000-0000-4000-8000-0000000000c${i + 1}`,
    name: `ymock-${n}`,
    kind: 'snmp_v2c',
  }));
  const ids = three.map((c) => c.id);
  let saved = false;
  let release: () => void = () => undefined;
  const held = new Promise<void>((r) => (release = r));
  await page.route('**/api/v1/credentials', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    // Every re-read after the first save is held, so the screen keeps drawing the old order.
    if (saved) await held;
    await route.fulfill({ json: three });
  });
  const puts: string[][] = [];
  page.on('request', (r) => {
    if (r.method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/credentials/order') {
      saved = true;
      puts.push((r.postDataJSON() as { ids: string[] }).ids);
    }
  });

  await page.goto('/nodes/credentials');
  const answered = () =>
    page.waitForResponse(
      (r) => r.request().method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/credentials/order',
    );
  let done = answered();
  await dragByKeys(page, page, 'ymock-c', -1);
  await done;
  // The dropped order stays on screen while the re-read is held (ADR-255), so the next drag starts
  // from it.
  await expect(page.locator('.dt-row .cred-order-n')).toHaveText(['1', '2', '3']);
  done = answered();
  await dragByKeys(page, page, 'ymock-c', -1);
  await done;
  release();
  expect(puts).toEqual([
    [ids[0], ids[2], ids[1]],
    [ids[2], ids[0], ids[1]],
  ]);
});

test('the grips go while the table is sorted by something else', async ({ page }) => {
  await page.goto('/nodes/credentials?sort=name');
  await expect(page.locator('.dt-row', { hasText: 'ymock-first' })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Reorder ymock-/ })).toHaveCount(0);
});

test('a scan sends the credentials in the order the picker shows them', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/discovery');
  // The credentials live in the folded settings (ADR-244). Set open rather than clicked: the
  // mock's pool has no live poller, which opens them by itself, and a click would close them.
  await page.locator('.disco-how').evaluate((d) => {
    (d as HTMLDetailsElement).open = true;
  });
  // A fresh browser remembers no scan, so every SNMP credential is ticked, in the server's order.
  const used = page.locator('.cred-picker-row:not(.unused) .cred-picker-row-name');
  await expect(used).toHaveText(['ymock-first', 'ymock-second']);
  await dragByKeys(page, page, 'ymock-second', -1);
  await expect(used).toHaveText(['ymock-second', 'ymock-first']);

  await page.locator('textarea.disco-range').fill('192.0.2.1');
  const post = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/discovery/scan',
  );
  await page.getByRole('button', { name: 'Scan', exact: true }).click();
  const body = (await post).postDataJSON() as { credential_ids: string[] };
  expect(body.credential_ids).toEqual([S2, S1]);
  expect(errors.uncaught).toEqual([]);
});

// ADR-205 Inc.7: every credential is one row that stays where it is. The checkbox says whether it
// is tried; the arrows, on every row, say in what order.
test('unticking leaves a credential in its place, and ticking it again keeps the place', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/discovery');
  // The credentials live in the folded settings (ADR-244). Set open rather than clicked: the
  // mock's pool has no live poller, which opens them by itself, and a click would close them.
  await page.locator('.disco-how').evaluate((d) => {
    (d as HTMLDetailsElement).open = true;
  });
  const names = page.locator('.cred-picker-row .cred-picker-row-name');
  const used = page.locator('.cred-picker-row:not(.unused) .cred-picker-row-name');
  await expect(names).toHaveText(['ymock-first', 'ymock-second']);

  await page.getByRole('checkbox', { name: 'Use ymock-first' }).uncheck();
  await expect(names).toHaveText(['ymock-first', 'ymock-second']);
  await expect(used).toHaveText(['ymock-second']);
  await expect(page.locator('.cred-picker-row:not(.unused) .cred-picker-row-n')).toHaveText(['1']);
  // An unticked row can still be moved, by its grip.
  await dragByKeys(page, page, 'ymock-first', 1);
  await expect(names).toHaveText(['ymock-second', 'ymock-first']);
  await dragByKeys(page, page, 'ymock-first', -1);
  await expect(names).toHaveText(['ymock-first', 'ymock-second']);

  await page.getByRole('checkbox', { name: 'Use ymock-first' }).check();
  await expect(names).toHaveText(['ymock-first', 'ymock-second']);
  await expect(used).toHaveText(['ymock-first', 'ymock-second']);

  await page.locator('textarea.disco-range').fill('192.0.2.1');
  const post = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/discovery/scan',
  );
  await page.getByRole('button', { name: 'Scan', exact: true }).click();
  const body = (await post).postDataJSON() as { credential_ids: string[] };
  expect(body.credential_ids).toEqual([S1, S2]);
  expect(errors.uncaught).toEqual([]);
});

// ADR-205 Inc.4: the browser remembers which credentials were ticked, never their order. A memory
// that lists them back to front still opens in the order the server lists them.
test('a remembered selection opens in the stored order, not the remembered one', async ({ page }) => {
  // Registered after the harness's own seeding script, so this one wins (see columnResize.spec.ts).
  await page.addInitScript(
    ([second, first]) => {
      localStorage.setItem(
        'yagra_prefs',
        JSON.stringify({
          state: {
            theme: 'dark',
            language: 'en',
            uiMode: 'auto',
            discoveryScan: {
              targetSpec: '',
              credentialIds: [second, first],
              pool: null,
              snmpWhenUnreachable: false,
            },
          },
          version: 0,
        }),
      );
    },
    [S2, S1],
  );
  await page.goto('/nodes/discovery');
  // The credentials live in the folded settings (ADR-244). Set open rather than clicked: the
  // mock's pool has no live poller, which opens them by itself, and a click would close them.
  await page.locator('.disco-how').evaluate((d) => {
    (d as HTMLDetailsElement).open = true;
  });
  await expect(page.locator('.cred-picker-row:not(.unused) .cred-picker-row-name')).toHaveText([
    'ymock-first',
    'ymock-second',
  ]);
});

// ADR-205 Inc.4: Detect remembers its ticks, and the range scan opens with them — the three pickers
// share one memory. Before, only a range scan wrote it, so a credential unticked for Detect came
// back the next time.
//
// Read back from storage rather than by navigating again: `tests/support/app.ts` re-seeds
// `yagra_prefs` on every navigation, so a second `goto` would show the harness's default, not this.
// What a fresh visit makes of the stored ids is the Vitest suite's (`initialCredentialIds`).
test('starting a Detect remembers the ticks it used', async ({ page }) => {
  await page.goto('/nodes/discovery?tab=unregistered');
  await page.getByRole('checkbox', { name: 'Use ymock-first' }).uncheck();
  await page.getByRole('button', { name: 'Detect' }).first().click();
  await expect
    .poll(() =>
      page.evaluate(
        () =>
          (JSON.parse(localStorage.getItem('yagra_prefs') ?? '{}') as {
            state?: { discoveryScan?: { credentialIds?: string[] } };
          }).state?.discoveryScan?.credentialIds ?? null,
      ),
    )
    .toEqual([S2]);
});

// A config backup with no order of its own tries the CLI logins in this order (ADR-205 Inc.5), so
// the CLI tab moves them the way the SNMP tab moves communities: within the tab, the rest in place.
test('moving a CLI login up saves the whole order, the SNMP rows where they stood', async ({
  page,
  errors,
}) => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  const C1 = '00000000-0000-4000-8000-0000000000d1';
  const C2 = '00000000-0000-4000-8000-0000000000d2';
  const mixed = [
    { ...first, id: S1, name: 'ymock-first', kind: 'snmp_v2c' },
    { ...first, id: C1, name: 'ymock-ops-login', kind: 'cli_login' },
    { ...first, id: S2, name: 'ymock-second', kind: 'snmp_v3' },
    { ...first, id: C2, name: 'ymock-admin-login', kind: 'cli_login' },
  ];
  await page.route('**/api/v1/credentials', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await route.fulfill({ json: mixed });
  });
  await page.goto('/nodes/credentials?tab=cli');
  // Every CLI login has its grip.
  await expect(grip(page, 'ymock-ops-login')).toBeVisible();
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/credentials/order',
  );
  await dragByKeys(page, page, 'ymock-admin-login', -1);
  const body = (await put).postDataJSON() as { ids: string[] };
  expect(body.ids).toEqual([S1, C2, S2, C1]);
  expect(errors.uncaught).toEqual([]);
});

// A CLI login the config backups try is not "Unused", and deleting it says it takes it away from
// them (ADR-205 Inc.6). The arithmetic is `credentialList.test.ts`'s; this is the cell and the
// dialog that read it.
test('a CLI login the config backups try says so, in the list and in the delete', async ({ page }) => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  const login = {
    ...first,
    id: '00000000-0000-4000-8000-0000000000d3',
    name: 'ymock-backup-login',
    kind: 'cli_login',
    used_by: 0,
    used_by_meraki_orgs: 0,
    used_by_netbox_servers: 0,
    used_by_config_backups: 4,
  };
  await page.route('**/api/v1/credentials', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await route.fulfill({ json: [login] });
  });
  await page.goto('/nodes/credentials?tab=cli');
  const row = page.locator('.dt-row').filter({ hasText: 'ymock-backup-login' });
  await expect(row).toContainText('4 config backups');
  await expect(row).not.toContainText('Unused');
  await row.hover();
  await row.getByRole('button', { name: 'Delete' }).click();
  await expect(page.getByRole('dialog')).toContainText('4 config backups reference it');
});
