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

test('moving an SNMP credential up saves the whole order, the HTTP row where it stood', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/credentials');
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/credentials/order',
  );
  await page.getByRole('button', { name: 'Move ymock-second up' }).click();
  const body = (await put).postDataJSON() as { ids: string[] };
  expect(body.ids).toEqual([S2, H1, S1]);
  expect(errors.uncaught).toEqual([]);
});

test('the arrows go while the table is sorted by something else', async ({ page }) => {
  await page.goto('/nodes/credentials?sort=name');
  await expect(page.locator('.dt-row', { hasText: 'ymock-first' })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Move ymock-/ })).toHaveCount(0);
});

test('a scan sends the credentials in the order the picker shows them', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/discovery');
  // A fresh browser remembers no scan, so every SNMP credential is ticked, in the server's order.
  const order = page.locator('.cred-picker-row-name');
  await expect(order).toHaveText(['ymock-first', 'ymock-second']);
  await page.getByRole('button', { name: 'Move ymock-second up' }).click();
  await expect(order).toHaveText(['ymock-second', 'ymock-first']);

  await page.locator('.disco-f-targets textarea').fill('192.0.2.1');
  const post = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/discovery/scan',
  );
  await page.getByRole('button', { name: 'Scan', exact: true }).click();
  const body = (await post).postDataJSON() as { credential_ids: string[] };
  expect(body.credential_ids).toEqual([S2, S1]);
  expect(errors.uncaught).toEqual([]);
});
