// SPDX-License-Identifier: AGPL-3.0-only
// Reading a device credential back on Credentials & secrets (ADR-210).
//
// WHY A BROWSER. Which rows offer "Show secret", and that opening it is what asks the server, are
// wired in `CredentialsPage.tsx`, which Vitest never runs. The route walk opens the screen and
// presses nothing, so the reveal request is never made there.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const V2C = '00000000-0000-4000-8000-0000000000d1';
const KEY = '00000000-0000-4000-8000-0000000000d2';

/** One SNMP community and one Meraki key, which must never offer to be shown. */
const credentials = (() => {
  const [first] = defaultBodyFor('/api/v1/credentials') as unknown as Schemas['CredentialSummary'][];
  return [
    { ...first, id: V2C, name: 'ymock-community', kind: 'snmp_v2c' },
    { ...first, id: KEY, name: 'ymock-meraki', kind: 'meraki_api' },
  ] as unknown as Json;
})();

const revealed = {
  id: V2C,
  kind: 'snmp_v2c',
  fields: [{ field: 'community', value: 'ymock-s3cr3t', secret: true }],
} satisfies Schemas['RevealedCredential'];

test.describe('with the reveal permission', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/credentials': credentials,
        '/api/v1/credentials/{id}/reveal': revealed as unknown as Json,
      },
    },
  });
  // Headless Chromium refuses a clipboard write unless it is granted, and the Copy button writes.
  test.use({ permissions: ['clipboard-read', 'clipboard-write'] });

  test('opening Show secret asks the server once and shows the value', async ({ page, errors }) => {
    await page.goto('/nodes/credentials');
    const asked: string[] = [];
    page.on('request', (r) => {
      const path = new URL(r.url()).pathname;
      if (path.endsWith('/reveal')) asked.push(`${r.method()} ${path}`);
    });
    await page.getByRole('button', { name: 'Show secret' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog.getByText('ymock-s3cr3t')).toBeVisible();
    await expect(dialog.getByText('Community', { exact: true })).toBeVisible();
    expect(asked).toEqual([`POST /api/v1/credentials/${V2C}/reveal`]);

    await dialog.getByRole('button', { name: 'Copy' }).click();
    await expect(dialog.getByRole('button', { name: 'Copied' })).toBeVisible();
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe('ymock-s3cr3t');

    // Closing drops the value: nothing of it stays on the page.
    await dialog.getByText('Close', { exact: true }).click();
    await expect(page.getByText('ymock-s3cr3t')).toHaveCount(0);
    expect(errors.uncaught).toEqual([]);
  });

  test('an integration key offers no Show secret', async ({ page }) => {
    await page.goto('/nodes/credentials?tab=integration');
    await expect(page.getByText('ymock-meraki')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Show secret' })).toHaveCount(0);
  });
});

test.describe('without the reveal permission', () => {
  // Every role holds everything in the bootstrap matrix; here none holds reveal_credentials.
  const roles = (() => {
    const base = BOOTSTRAP_OVERRIDES['/api/v1/roles'] as unknown as {
      permissions: { key: string }[];
      roles: { key: string; permissions: string[] }[];
    };
    return {
      ...base,
      roles: base.roles.map((r) => ({
        ...r,
        permissions: r.permissions.filter((p) => p !== 'reveal_credentials'),
      })),
    } as unknown as Json;
  })();
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/credentials': credentials,
        '/api/v1/roles': roles,
      },
    },
  });

  test('the row draws Edit and Delete and no Show secret', async ({ page }) => {
    await page.goto('/nodes/credentials');
    await expect(page.getByText('ymock-community')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Edit' })).toHaveCount(1);
    await expect(page.getByRole('button', { name: 'Show secret' })).toHaveCount(0);
  });
});
