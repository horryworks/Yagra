// SPDX-License-Identifier: AGPL-3.0-only
// The visual notification-template editor (ADR-039 Inc.2).
//
// Why Tier1: every decision is in a `.ts` with tests - the model (`templateModel.ts`), the field's
// DOM (`templateDom.ts`, under jsdom). What only a browser shows is the pieces handed to each other:
// the built-in draft drawn as tags in a real `contenteditable`, "{" typed into it opening the list,
// a pick landing at the caret, the preview asked for the sample the operator chose, and the save
// carrying what the "Template that will be saved" panel showed.
//
// The walk cannot be the reader: it opens Notification delivery and no dialog. The bodies are the
// generated ones, patched - a hand-written fixture would be a second copy of the contract
// (testing.md). The preview is patched to carry no `json_valid`, which is how the server says a
// channel's body is plain text; the generated body names it, which would read as a JSON channel.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const CHANNEL_ID = '00000000-0000-4000-8000-0000000000c1';

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
    },
  ] as unknown as Json;
})();

/** What the server answers today: the built-in node subject, per point in the alert's life. */
const builtin: Schemas['BuiltinSubjectTemplate'][] = [
  { event: 'fire', subject: 'node {{ node_id }} is {{ state }}' },
  { event: 'resolve', subject: 'resolved: node {{ node_id }} recovered' },
  { event: 'suppress', subject: 'rolled up: node {{ node_id }} suppressed under upstream' },
];

const preview = {
  subject: 'node 6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60 is unreachable',
  body: '{"node":"6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60"}',
  problems: [],
} as unknown as Json;

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/notification-channels': channels,
      '/api/v1/notification-channels/builtin-template': builtin as unknown as Json,
      '/api/v1/notification-channels/preview': preview,
    },
  },
});

test('the built-in subject opens as tags, "{" inserts a variable, and the save carries what was shown', async ({
  page,
  errors,
}) => {
  await page.goto('/alerts/routing');
  const row = page.locator('.dt-row').filter({ hasText: 'ymock-jsm' });
  await row.hover();
  await row.getByRole('button', { name: 'Edit notification template' }).click();
  const dialog = page.getByRole('dialog');

  // The built-in subject is the draft, drawn as tags in the operator's language.
  const subject = dialog.locator('#tpl-subject');
  await expect(subject.locator('.tpl-chip')).toHaveText(['Node ID', 'State']);
  await expect(subject).toContainText('node');
  await expect(dialog.getByRole('button', { name: 'Visual' })).toHaveAttribute('aria-pressed', 'true');
  await expect(dialog).toContainText('This is Yagra’s built-in subject');

  // Typing "{" in the body opens the list at the caret; Enter picks the first match.
  const body = dialog.locator('#tpl-body');
  await body.click();
  await page.keyboard.type('Down: ');
  await page.keyboard.type('{');
  const picker = page.getByRole('listbox', { name: 'Variables' });
  await expect(picker).toBeVisible();
  await page.keyboard.type('subject name');
  await page.keyboard.press('Enter');
  await expect(picker).toBeHidden();
  await expect(body.locator('.tpl-chip')).toHaveText(['Subject name']);

  // The panel says what will be saved: the subject untouched stays the built-in.
  await dialog.getByText('Template that will be saved').click();
  const saved = dialog.locator('.tpl-saved pre');
  await expect(saved.nth(0)).toHaveText('(empty = built-in text)');
  await expect(saved.nth(1)).toHaveText('Down: {{ subject_name }}');

  // Choosing a sample asks the preview for that sample and moves to its tab.
  const asked = page.waitForRequest(
    (r) =>
      r.method() === 'POST' &&
      new URL(r.url()).pathname === '/api/v1/notification-channels/preview' &&
      (r.postDataJSON() as { event?: string }).event === 'resolve',
  );
  await dialog.getByRole('button', { name: 'Node recovered' }).click();
  const req = (await asked).postDataJSON() as Record<string, unknown>;
  expect(req).toMatchObject({ kind: 'jsm', event: 'resolve', sample: 'liveness', subject: null });
  await expect(dialog.getByRole('tab', { name: 'When it recovers' })).toHaveAttribute('aria-selected', 'true');

  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === `/api/v1/notification-channels/${CHANNEL_ID}/template`,
  );
  await dialog.getByRole('button', { name: 'Save template' }).click();
  expect((await put).postDataJSON()).toEqual({ subject: null, body: 'Down: {{ subject_name }}' });

  expect(errors.uncaught).toEqual([]);
});
