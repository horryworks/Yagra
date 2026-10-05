// SPDX-License-Identifier: AGPL-3.0-only
// Notification templates a person can read (ADR-199): the built-in subject, now written with
// `node_label` / `alert_label` and a prefix that is sent only with its value, opens as tags; and the
// code view's free layout lays the built-in copy out over several lines and saves the switch with it.
//
// Why Tier1: the reading and the layout are decided in `.ts` files with tests (`templateModel.ts`,
// `templateDisplay.ts`, `channelTemplate.ts`). What only a browser shows is them handed to each
// other: the tags drawn in a real `contenteditable` with the prefix inside the tag, the subject field
// turning into a multi-line one, and the save carrying `free_layout`. Whether the laid-out copy sends
// the built-in text is the server's rendering, pinned by `alerts/notify.rs`'s
// `the_laid_out_builtin_sends_the_builtin` — this mock cannot render.

import type { Page } from '@playwright/test';
import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const CHANNEL_ID = '00000000-0000-4000-8000-0000000000d1';

const channels = (() => {
  const [first] = defaultBodyFor('/api/v1/notification-channels') as unknown as Schemas['ChannelSummary'][];
  return [
    {
      ...first,
      id: CHANNEL_ID,
      name: 'ymock-jsm',
      kind: 'jsm',
      enabled: true,
      subject_template: null,
      body_template: null,
      template_free_layout: false,
    },
  ] as unknown as Json;
})();

/** The built-in subject as `notify_text.rs::node_subject_template` writes it since ADR-199. */
const FIRE = '{{ node_label }} is {{ state }}{% if alert_label is defined %}: {{ alert_label }}{% endif %}';
const RESOLVE = 'resolved: {{ node_label }} recovered';
const SUPPRESS =
  'rolled up: {{ node_label }} suppressed under upstream{% if root_cause_name is defined %} {{ root_cause_name }}{% endif %}';
const LINES = '\n\nSeverity:  {{ severity }}\n{% if group is defined %}Folder:    {{ group }}\n{% endif %}\n';
const builtin: Schemas['BuiltinSubjectTemplate'][] = [
  { event: 'fire', subject: FIRE, body: `${FIRE}${LINES}` },
  { event: 'resolve', subject: RESOLVE, body: `${RESOLVE}${LINES}` },
  { event: 'suppress', subject: SUPPRESS, body: `${SUPPRESS}${LINES}` },
];

const variables: Schemas['TemplateVariable'][] = [
  { name: 'node_label', description: 'node with address', always_present: true },
  { name: 'state', description: 'state', always_present: true },
  { name: 'alert_label', description: 'alert with where', always_present: false },
  { name: 'root_cause_name', description: 'upstream', always_present: false },
];

const preview = {
  subject: 'core-sw-01 (192.0.2.11) is critical: Inbound utilization on GigabitEthernet0/7',
  body: 'core-sw-01 (192.0.2.11) is critical',
  problems: [],
} as unknown as Json;

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/notification-channels': channels,
      '/api/v1/notification-channels/builtin-template': builtin as unknown as Json,
      '/api/v1/notification-channels/preview': preview,
      '/api/v1/notification-channels/template-variables': variables as unknown as Json,
    },
  },
});

async function openTemplate(page: Page) {
  await page.goto('/alerts/routing');
  const row = page.locator('.dt-row').filter({ hasText: 'ymock-jsm' });
  await row.hover();
  await row.getByRole('button', { name: 'Edit notification template' }).click();
  return page.getByRole('dialog');
}

test('the built-in title opens as tags, with the text sent only alongside its value inside the tag', async ({
  page,
  errors,
}) => {
  const dialog = await openTemplate(page);
  await dialog.getByRole('button', { name: 'Edit the Title as tags' }).click();
  await expect(dialog.getByRole('button', { name: 'Visual' })).toHaveAttribute('aria-pressed', 'true');

  const chips = dialog.locator('.tpl-chip');
  await expect(chips.first()).toContainText('Node with address');
  const alert = chips.filter({ hasText: 'Alert name with where' });
  await expect(alert.locator('.tpl-chip-prefix')).toHaveText(': ');

  // The prefix is edited on the tag, and the save writes it inside the condition.
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === `/api/v1/notification-channels/${CHANNEL_ID}/template`,
  );
  await alert.click();
  await dialog.locator('#tpl-missing-prefix-text').fill(' — ');
  await expect(alert.locator('.tpl-chip-prefix')).toHaveText(' — ');
  await page.keyboard.press('Escape');
  await dialog.getByRole('button', { name: 'Save template' }).click();
  const sent = (await put).postDataJSON() as { subject: string; body: string | null };
  expect(sent.subject).toContain('{% if alert_label is defined %} — {{ alert_label }}{% endif %}');
  expect(sent.body).toBeNull();

  expect(errors.uncaught).toEqual([]);
});

test('"Start from empty fields" is empty, now that the built-in title reads as tags', async ({ page, errors }) => {
  const dialog = await openTemplate(page);
  await dialog.getByRole('button', { name: 'Start from empty fields' }).click();
  await expect(dialog.getByRole('button', { name: 'Visual' })).toHaveAttribute('aria-pressed', 'true');
  await expect(dialog.locator('.tpl-chip')).toHaveCount(0);
  expect(errors.uncaught).toEqual([]);
});

test('free layout lays the built-in copy out over several lines and is saved with it', async ({ page, errors }) => {
  const dialog = await openTemplate(page);
  await dialog.getByRole('button', { name: 'Edit a copy of this text' }).click();
  await expect(dialog.locator('#tpl-subject')).toHaveJSProperty('tagName', 'INPUT');
  const oneLine = await dialog.locator('#tpl-subject').inputValue();
  expect(oneLine).not.toContain('\n');

  const layout = dialog.getByRole('checkbox', { name: /Free layout/ });
  await layout.check();
  // The subject becomes a multi-line field holding the same text laid out.
  await expect(dialog.locator('#tpl-subject')).toHaveJSProperty('tagName', 'TEXTAREA');
  const laid = await dialog.locator('#tpl-subject').inputValue();
  expect(laid.split('\n')).toEqual([
    '{% if event == "resolve" %}',
    `  ${RESOLVE}`,
    '{% elif event == "suppress" %}',
    `  ${SUPPRESS}`,
    '{% else %}',
    `  ${FIRE}`,
    '{% endif %}',
  ]);
  await expect(dialog.getByText(/the spaces at the start of a line are not sent/)).toBeVisible();

  // The preview is asked for with the switch, and the save carries it.
  const asked = page.waitForRequest(
    (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/notification-channels/preview',
  );
  expect(((await asked).postDataJSON() as { free_layout?: boolean }).free_layout).toBe(true);
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === `/api/v1/notification-channels/${CHANNEL_ID}/template`,
  );
  await dialog.getByRole('button', { name: 'Save template' }).click();
  const sent = (await put).postDataJSON() as { subject: string; free_layout?: boolean };
  expect(sent.subject).toBe(laid);
  expect(sent.free_layout).toBe(true);
  expect(errors.uncaught).toEqual([]);
});
