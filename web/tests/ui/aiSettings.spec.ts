// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ AI analysis after ADR-200 Inc.8: what the screen does in place of the sentences it
// lost.
//
// 🚨 The first test is the one that matters. The page used to say, always, that node names,
// addresses, topology and syslog lines leave the operator's boundary for a vendor provider. That
// paragraph went only because switching sending on now asks first — so if the dialog stops
// appearing, the warning is gone from the product while the data still leaves.
//
// The route walk opens this screen once and never presses Save, so none of this is visible to it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

/** A stored Claude configuration with a key, sending switched off; three providers, one of which
 *  keeps the data inside the operator's cloud and one whose address is typed (ADR-247). Patched onto the generated body so the fixture
 *  follows `LlmConfigResponse` rather than transcribing it (ADR-052 decision 2). */
function llmConfig(): Json {
  const body = defaultBodyFor('/api/v1/llm/config') as {
    config?: Record<string, unknown> | null;
    providers: Record<string, unknown>[];
  };
  const choice = body.providers[0] ?? {};
  body.providers = [
    {
      ...choice,
      key: 'vertex',
      suggested_model: 'gemini-2.5-pro',
      suggested_location: 'asia-northeast1',
      leaves_operator_boundary: false,
      needs_project: true,
      needs_base_url: false,
      credential_optional: true,
    },
    {
      ...choice,
      key: 'claude',
      suggested_model: 'claude-sonnet-4-5',
      suggested_location: null,
      leaves_operator_boundary: true,
      needs_project: false,
      needs_base_url: false,
      credential_optional: false,
    },
    {
      ...choice,
      key: 'local',
      suggested_model: 'qwen3',
      suggested_location: null,
      leaves_operator_boundary: true,
      needs_project: false,
      needs_base_url: true,
      credential_optional: true,
    },
  ];
  body.config = {
    ...(body.config ?? {}),
    provider: 'claude',
    model: 'claude-sonnet-4-5',
    project: '',
    location: '',
    base_url: '',
    enabled: false,
    max_output_tokens: 8192,
    has_api_key: true,
    leaves_operator_boundary: true,
    updated_at: '2026-10-01T00:00:00Z',
  };
  return body as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/llm/config': () => llmConfig(),
    },
  },
});

const puts = (mock: { requests: { method: string; pathname: string }[] }) =>
  mock.requests.filter((r) => r.method === 'PUT' && r.pathname === '/api/v1/llm/config');

test('switching sending on for a vendor outside the boundary asks first', async ({ page, mock }) => {
  await page.goto('/settings/ai');
  await expect(page.locator('#ai-model')).toHaveValue('claude-sonnet-4-5');

  await page.getByRole('checkbox', { name: 'Enable AI-assisted analysis' }).check();
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const dialog = page.locator('[aria-modal="true"]');
  await expect(dialog).toContainText('outside your boundary');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
  expect(puts(mock), 'cancelling must not switch sending on').toHaveLength(0);

  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('button', { name: 'Save and enable' }).click();
  await expect(dialog).toHaveCount(0);
  await expect.poll(() => puts(mock).length).toBe(1);
});

test('a save that leaves sending off is not asked about', async ({ page, mock }) => {
  await page.goto('/settings/ai');
  await page.locator('#ai-model').fill('claude-opus-4-1');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(() => puts(mock).length).toBe(1);
  await expect(page.locator('[aria-modal="true"]')).toHaveCount(0);
});

test('Test waits for a save, and the stored key is kept unless replaced', async ({ page }) => {
  await page.goto('/settings/ai');
  const testButton = page.getByRole('button', { name: 'Test', exact: true });
  await expect(testButton).toBeEnabled();

  // The key is shown as stored, with no box to type over it by accident.
  await expect(page.locator('.secret-input-stored')).toContainText('Stored');
  await expect(page.locator('#ai-key')).toHaveCount(0);

  // An unsaved edit: the test would describe the stored configuration, not this one.
  await page.locator('#ai-model').fill('claude-opus-4-1');
  await expect(testButton).toBeDisabled();
  await page.locator('#ai-model').fill('claude-sonnet-4-5');
  await expect(testButton).toBeEnabled();

  // The output limit carries its unit and range in place of a sentence.
  await expect(page.locator('.field-suffix')).toHaveText('tokens');
  await expect(page.locator('.ui-setting').filter({ has: page.locator('#ai-tokens') })).toContainText(
    '(256–65536)',
  );
});

test("an operator's own server asks for its address, and the confirm names the host", async ({
  page,
  mock,
}) => {
  await page.goto('/settings/ai');
  await page.locator('#ai-provider').selectOption('local');
  await page.locator('#ai-model').fill('qwen3');
  await page.getByRole('checkbox', { name: 'Enable AI-assisted analysis' }).check();

  // No address: refused where it was typed, nothing sent.
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect(page.getByText('Enter the server URL')).toBeVisible();
  expect(puts(mock)).toHaveLength(0);

  // An address outside: the confirm says where the data goes.
  await page.locator('#ai-base-url').fill('https://llm.example.com/v1');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  const dialog = page.locator('[aria-modal="true"]');
  await expect(dialog).toContainText('llm.example.com');
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  expect(puts(mock)).toHaveLength(0);

  // A private literal: saved without asking.
  await page.locator('#ai-base-url').fill('http://10.0.0.1:8080/v1');
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await expect.poll(() => puts(mock).length).toBe(1);
  await expect(dialog).toHaveCount(0);
});