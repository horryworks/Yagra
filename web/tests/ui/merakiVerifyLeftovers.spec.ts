// SPDX-License-Identifier: AGPL-3.0-only
// Three pieces of wiring from ADR-164 Inc.11 that only a browser can see.
//
// WHY A BROWSER. The judgement behind each one is unit-tested (`merakiCadence.ts`,
// `credentialKinds.ts`), but what the fix *is* lives in a `.tsx`, which Vitest never runs
// (testing.md): the order two promises resolve in, which control a dialog draws, and what a press
// sends. Each of these shipped wrong with every other test green.
//
// The bodies are the generated ones, patched — a hand-written fixture would be a second copy of the
// contract (testing.md). The writes pressed here are answered by the mock.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { MERAKI_ORG_SCREEN } from './screens';

type Schemas = components['schemas'];

const STORED_CAP = 500;
const TYPED_CAP = 777;
const MERAKI_KEY_NAME = 'ymock-meraki-key';

const orgWithCap = (cap: number): Json => {
  const body = defaultBodyFor('/api/v1/meraki/orgs') as unknown as Schemas['MerakiOrgView'][];
  body[0] = { ...body[0], name: 'ymock-acme', max_devices: cap, collect_failures: [] };
  return body as unknown as Json;
};

/** One stored key an integration owns, and one made on the Credentials page. */
const credentials = (() => {
  const [first] = defaultBodyFor(
    '/api/v1/credentials',
  ) as unknown as Schemas['CredentialSummary'][];
  return [
    { ...first, id: '00000000-0000-4000-8000-0000000000a1', name: MERAKI_KEY_NAME, kind: 'meraki_api' },
    { ...first, id: '00000000-0000-4000-8000-0000000000a2', name: 'ymock-community', kind: 'snmp_v2c' },
  ] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/meraki/orgs': orgWithCap(STORED_CAP),
      '/api/v1/credentials': credentials,
    },
  },
});

// The form follows the server until it is touched, and again once a save's reload has ARRIVED.
// It used to start following at the moment of the save, while `org` was still the old one — so the
// cap just saved flicked back to the stored one for as long as the reload took (two round trips and
// the whole device list). The reload is held open here, which is the window the defect lived in.
test('a saved cap stays on screen while the page reloads', async ({ page, errors }) => {
  await page.goto(MERAKI_ORG_SCREEN);
  const cap = page.locator('.meraki-orgpage-settings input[type="number"]');
  await expect(cap).toHaveValue(String(STORED_CAP));

  // Registered after the page has loaded, so the only `GET …/meraki/orgs` it can meet is the
  // reload the save asks for. That one is held, then answered with what was saved.
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route('**/api/v1/meraki/orgs', async (route) => {
    if (route.request().method() !== 'GET') return route.fallback();
    await held;
    return route.fulfill({ json: orgWithCap(TYPED_CAP) });
  });

  await cap.fill(String(TYPED_CAP));
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname.endsWith('/import-settings'),
  );
  await page.getByRole('button', { name: 'Save' }).click();
  expect(((await put).postDataJSON() as { max_devices: number }).max_devices).toBe(TYPED_CAP);

  // The reload is in flight and held. The typed value is still what the box says…
  await expect(cap).toHaveValue(String(TYPED_CAP));
  // …and it is locked, so nothing can be typed into a form about to follow the server.
  await expect(cap).toBeDisabled();

  release();
  await expect(cap).toBeEnabled();
  await expect(cap).toHaveValue(String(TYPED_CAP));
  // Following again: nothing is edited, so there is nothing to save.
  await expect(page.getByRole('button', { name: 'Save' })).toBeDisabled();
  expect(errors.uncaught).toEqual([]);
});

// An integration's key keeps its kind. The dialog drew its four-kind select over a Meraki key: the
// select had no such option, so it SHOWED "SNMP v2c" while the state said `meraki_api`. Left alone
// it stored the bare key where a JSON document belongs; touched, the key became a community. Either
// way the organization's collects and syncs failed with `credential` from then on.
test('replacing a Meraki key keeps its kind and sends the document the server parses', async ({
  page,
  errors,
}) => {
  await page.goto('/nodes/credentials');
  const row = page.locator('.dt-row', { hasText: MERAKI_KEY_NAME });
  await row.hover();
  await row.getByRole('button', { name: 'Edit' }).click();

  const dialog = page.getByRole('dialog');
  await dialog.getByRole('checkbox').check();
  // No select to choose another kind with — the kind is said, not offered.
  await expect(dialog.getByRole('combobox')).toHaveCount(0);
  await expect(dialog).toContainText('Cisco Meraki API key');
  await expect(dialog).toContainText(
    'This key belongs to an integration, so its type cannot be changed.',
  );

  await dialog.locator('input[type="password"]').fill('  rotated-key\n');
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname.includes('/credentials/'),
  );
  await dialog.getByRole('button', { name: 'Save' }).click();
  const body = (await put).postDataJSON() as { name: string; kind: string; secret: string };
  expect(body.kind).toBe('meraki_api');
  expect(JSON.parse(body.secret)).toEqual({ api_key: 'rotated-key' });
  expect(errors.uncaught).toEqual([]);
});

// The accept side: a kind made on this page still gets the select, starting on its own kind.
test('a credential made here can still change kind', async ({ page }) => {
  await page.goto('/nodes/credentials');
  const row = page.locator('.dt-row', { hasText: 'ymock-community' });
  await row.hover();
  await row.getByRole('button', { name: 'Edit' }).click();
  const dialog = page.getByRole('dialog');
  await dialog.getByRole('checkbox').check();
  await expect(dialog.getByRole('combobox')).toHaveValue('snmp_v2c');
});

// The ranges under the cadence dialog's intervals come from `merakiCadence.ts`, which a Rust test
// holds to the server's bounds. They were four literals in the `.tsx`; what is wiring is that each
// interval is drawn with ITS band, as the hint and as the input's own min and max.
test('each cadence interval is drawn with its own band', async ({ page }) => {
  await page.goto('/settings/integrations/meraki');
  await page.getByRole('button', { name: 'Cadence' }).first().click();
  const dialog = page.getByRole('dialog');
  const field = (label: string) => dialog.locator('.modal-field', { hasText: label });
  for (const [label, min, max] of [
    ['Availability interval (s)', '60', '3600'],
    ['Uplink interval (s)', '60', '3600'],
    ['Traffic interval (s)', '300', '86400'],
    ['Device sync interval (s)', '60', '604800'],
  ] as const) {
    const input = field(label).locator('input[type="number"]');
    await expect(input, label).toHaveAttribute('min', min);
    await expect(input, label).toHaveAttribute('max', max);
    await expect(field(label).locator('.modal-hint'), label).toHaveText(`${min}–${max}`);
  }
});
