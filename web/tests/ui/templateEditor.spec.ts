// SPDX-License-Identifier: AGPL-3.0-only
// The visual notification-template editor (ADR-039 Inc.2), and what it shows a channel that has
// no template of its own: the built-in text, read-only, with a way to edit a copy, and a way back
// to the built-in for one that has (ADR-197).
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

import type { Page } from '@playwright/test';
import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const CHANNEL_ID = '00000000-0000-4000-8000-0000000000c1';
/** A channel that already sends a template of its own. */
const OWN_ID = '00000000-0000-4000-8000-0000000000c2';
const HOOK_ID = '00000000-0000-4000-8000-0000000000c3';
const PD_ID = '00000000-0000-4000-8000-0000000000c4';

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
    {
      ...first,
      id: OWN_ID,
      name: 'ymock-mail',
      kind: 'email',
      enabled: true,
      subject_template: '{{ subject_name }} is {{ state }}',
      body_template: null,
    },
    { ...first, id: HOOK_ID, name: 'ymock-hook', kind: 'webhook', enabled: true, subject_template: null, body_template: null },
    { ...first, id: PD_ID, name: 'ymock-pd', kind: 'pagerduty', enabled: true, subject_template: null, body_template: null },
  ] as unknown as Json;
})();

/** The shape the server answers for JSM: per point in the alert's life, a subject with a part
 *  sent only under a condition, and a body that starts with the same sentence. Shortened from
 *  `notify_text.rs`, keeping the shapes the visual editor cannot read. */
const WHO = '{{ node_name }}{% if node_address and node_address != node_name %} ({{ node_address }}){% endif %}';
const LINES = '\n\nSeverity:  {{ severity }}\n{% if group is defined %}Folder:    {{ group }}\n{% endif %}\n';
const builtin: Schemas['BuiltinSubjectTemplate'][] = [
  { event: 'fire', subject: `${WHO} is {{ state }}`, body: `${WHO} is {{ state }}${LINES}` },
  { event: 'resolve', subject: `resolved: ${WHO} recovered`, body: `resolved: ${WHO} recovered${LINES}` },
  { event: 'suppress', subject: `rolled up: ${WHO}`, body: `rolled up: ${WHO}${LINES}` },
];

async function openTemplate(page: Page, channel: string) {
  await page.goto('/alerts/routing');
  const row = page.locator('.dt-row').filter({ hasText: channel });
  await row.hover();
  await row.getByRole('button', { name: 'Edit notification template' }).click();
  return page.getByRole('dialog');
}

/** Two real names, so the tooltip reads the locale strings a real list would. */
const variables: Schemas['TemplateVariable'][] = [
  { name: 'subject_name', description: 'subject name', always_present: true },
  { name: 'state', description: 'state', always_present: true },
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
      '/api/v1/notification-channels/template-variables': variables as unknown as Json,
    },
  },
});

test('a channel with no template shows the built-in text, and editing a copy saves exactly that text', async ({
  page,
  errors,
}) => {
  const dialog = await openTemplate(page, 'ymock-jsm');
  await expect(dialog.getByRole('status').first()).toContainText('Sending Yagra’s built-in text');

  // Variables as tags; the part sent only when the address is not the name, in a box that says so.
  const shown = dialog.locator('#tpl-builtin-subject');
  await expect(shown.locator('.tpl-chip')).toHaveText(['Node name', 'Address', 'State']);
  // The conditional part is numbered in the sentence and its condition said once, below.
  await expect(shown.locator('.tpl-cond-part')).toHaveText(['(Address)']);
  await expect(dialog.locator('.tpl-legend').first()).toHaveText('①only when Address differs from Node name');
  const shownBody = dialog.locator('#tpl-builtin-body');
  await expect(shownBody).toContainText('Severity:');
  // The title sentence it repeats is one tag; a whole kept-or-dropped line says so at its end.
  await expect(shownBody.locator('.tpl-chip.is-lead')).toHaveText('Same sentence as the title');
  await expect(shownBody.locator('.tpl-line-when')).toHaveText(['only when Folder is known']);

  // Each tab shows its own point in the alert's life.
  await dialog.getByRole('tab', { name: 'When it recovers' }).click();
  await expect(dialog.locator('#tpl-builtin-subject')).toContainText('resolved:');

  // The copy opens in the code editor, one template branching on the event.
  await dialog.getByRole('button', { name: 'Edit a copy of this text' }).click();
  await expect(dialog.getByRole('button', { name: 'Code' })).toHaveAttribute('aria-pressed', 'true');
  const copied = await dialog.locator('#tpl-subject').inputValue();
  expect(copied).toContain('{% if event == "resolve" %}resolved: ');
  expect(copied).toContain('{% else %}{{ node_name }}');
  // The body's lines are not repeated per event: only its first line branches.
  const copiedBody = await dialog.locator('#tpl-body').inputValue();
  expect(copiedBody.match(/Severity:/g)).toHaveLength(1);

  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === `/api/v1/notification-channels/${CHANNEL_ID}/template`,
  );
  await dialog.getByRole('button', { name: 'Save template' }).click();
  expect((await put).postDataJSON()).toEqual({ subject: copied, body: copiedBody });

  expect(errors.uncaught).toEqual([]);
});

test('a webhook shows the JSON it posts and no subject, and starts from a JSON skeleton', async ({ page, errors }) => {
  const dialog = await openTemplate(page, 'ymock-hook');
  await expect(dialog).toContainText('No subject is sent.');
  await expect(dialog.locator('#tpl-builtin-subject')).toHaveCount(0);
  // The built-in body is the preview of the alert itself, laid out one value per line.
  const body = dialog.locator('#tpl-builtin-body');
  await expect(body).toContainText('"node": "6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60"');
  await expect(body.locator('.tpl-json-key')).toHaveText(['"node"']);
  await dialog.getByText('What each key means').click();
  await expect(dialog.locator('.tpl-keys')).toContainText('The ID of the check that raised the alert.');

  await dialog.getByRole('button', { name: 'Start from a JSON skeleton' }).click();
  await expect(dialog.locator('#tpl-subject')).toHaveCount(0);
  await expect(dialog.locator('#tpl-body')).toHaveValue(/"dedup_key": \{\{ dedup_key \| tojson \}\}/);
  expect(errors.uncaught).toEqual([]);
});

test('PagerDuty says that a recovery sends only a close signal', async ({ page, errors }) => {
  const dialog = await openTemplate(page, 'ymock-pd');
  await expect(dialog.locator('#tpl-builtin-subject')).toBeVisible();
  await dialog.getByRole('tab', { name: 'When it recovers' }).click();
  await expect(dialog.locator('.tpl-edit')).toContainText('PagerDuty is sent only a close signal');
  await expect(dialog.locator('#tpl-builtin-subject')).toHaveCount(0);
  await expect(dialog.locator('.tpl-preview')).toContainText('"event_action": "resolve"');
  expect(errors.uncaught).toEqual([]);
});

test('a channel with its own template can go back to the built-in text, after saying so', async ({ page, errors }) => {
  const dialog = await openTemplate(page, 'ymock-mail');
  const status = dialog.getByRole('status').first();
  await expect(status).toContainText('Sending this channel’s own template');
  // The consent is the shared destructive-confirmation dialog, stacked over the editor.
  await dialog.getByRole('button', { name: 'Go back to built-in text…' }).click();
  const confirm = page.getByRole('dialog', { name: 'Delete this channel’s template and send the built-in text again?' });
  await expect(confirm).toBeVisible();

  // Cancel changes nothing.
  await confirm.getByRole('button', { name: 'Cancel' }).click();
  await expect(confirm).toBeHidden();
  await expect(status).toContainText('Sending this channel’s own template');

  await dialog.getByRole('button', { name: 'Go back to built-in text…' }).click();
  const put = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname === `/api/v1/notification-channels/${OWN_ID}/template`,
  );
  await confirm.getByRole('button', { name: 'Delete and use built-in' }).click();
  expect((await put).postDataJSON()).toEqual({ subject: null, body: null });

  expect(errors.uncaught).toEqual([]);
});

test('starting from empty fields, "{" inserts a variable, and the save carries what was shown', async ({
  page,
  errors,
}) => {
  const dialog = await openTemplate(page, 'ymock-jsm');
  await dialog.getByRole('button', { name: 'Start from empty fields' }).click();
  await expect(dialog.getByRole('button', { name: 'Visual' })).toHaveAttribute('aria-pressed', 'true');

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

test('in the code view a variable goes where the caret is, and hovering one says what it is', async ({ page, errors }) => {
  const dialog = await openTemplate(page, 'ymock-jsm');
  await dialog.getByRole('button', { name: 'Start from empty fields' }).click();
  await dialog.getByRole('button', { name: 'Code' }).click();

  const subject = dialog.locator('#tpl-subject');
  const body = dialog.locator('#tpl-body');
  await subject.fill('Down: now');
  await body.fill('body text');
  // Caret after "Down: " in the subject; the body was the last field typed in before this.
  await subject.click();
  await page.keyboard.press('End');
  for (let i = 0; i < 3; i++) await page.keyboard.press('ArrowLeft');

  const stateButton = dialog.locator('.tpl-var').filter({ hasText: 'state' });
  await stateButton.click();
  await expect(subject).toHaveValue('Down: {{ state }}now');
  await expect(body).toHaveValue('body text');
  // Focus stayed in the subject, with the caret after the insert, so typing carries on there.
  await page.keyboard.type('!');
  await expect(subject).toHaveValue('Down: {{ state }}!now');

  // Hovering a variable shows what it is, in the operator's language.
  await dialog.locator('.tpl-var').filter({ hasText: 'subject_name' }).hover();
  const tip = page.getByRole('tooltip');
  await expect(tip).toContainText('Subject name');
  await expect(tip).toContainText('The node’s name, or the poller pool’s');
  await expect(tip).toContainText('{{ subject_name }}');
  // Read the computed style: isVisible() counts a hidden-but-laid-out element as visible.
  expect(await tip.evaluate((el) => getComputedStyle(el).visibility)).toBe('visible');
  await page.mouse.move(0, 0);
  await expect(tip).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
});

// --- Resizing the dialog (ADR-198) -------------------------------------------------------------
// The arithmetic is `lib/modalSize.ts`'s, under Vitest. What only a browser shows: the edge staying
// under the pointer while the centred dialog grows on both sides, the size coming back on the next
// open, the two columns folding when the DIALOG (not the window) gets narrow, and the handles going
// away on a phone.

const widthHandle = (dialog: import('@playwright/test').Locator) =>
  dialog.getByRole('slider', { name: "Resize the dialog's width" });
const heightHandle = (dialog: import('@playwright/test').Locator) =>
  dialog.getByRole('slider', { name: "Resize the dialog's height" });

async function dragBy(page: Page, handle: import('@playwright/test').Locator, dx: number, dy: number) {
  const box = await handle.boundingBox();
  if (!box) throw new Error('the handle has no box');
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + dx, y + dy, { steps: 6 });
  await page.mouse.up();
  return { x: x + dx, y: y + dy };
}

/** Open the dialog again WITHOUT navigating: a `goto` reloads the page, and the fixture's init
 *  script re-seeds `yagra_prefs`, which would erase the very size this asks to be remembered. */
async function reopenTemplate(page: Page, channel: string) {
  const row = page.locator('.dt-row').filter({ hasText: channel });
  await row.hover();
  await row.getByRole('button', { name: 'Edit notification template' }).click();
  return page.getByRole('dialog');
}

async function widthOf(dialog: import('@playwright/test').Locator) {
  return (await dialog.boundingBox())?.width ?? 0;
}

test('the template dialog widens from its right edge, keeps the edge under the pointer, and remembers it', async ({
  page,
  errors,
}) => {
  let dialog = await openTemplate(page, 'ymock-jsm');
  const before = await widthOf(dialog);
  const at = await dragBy(page, widthHandle(dialog), 100, 0);
  // Centred, so the dialog grows by twice the travel and its edge follows the pointer.
  expect(Math.abs((await widthOf(dialog)) - (before + 200))).toBeLessThanOrEqual(2);
  const edge = await widthHandle(dialog).boundingBox();
  expect(Math.abs((edge?.x ?? 0) + (edge?.width ?? 0) / 2 - at.x)).toBeLessThanOrEqual(3);

  // Closed and opened again, it comes back at the width it was left at.
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  dialog = await reopenTemplate(page, 'ymock-jsm');
  expect(Math.abs((await widthOf(dialog)) - (before + 200))).toBeLessThanOrEqual(2);

  // Double-click goes back to the default; an arrow key moves it one step.
  await widthHandle(dialog).dblclick();
  await expect.poll(() => widthOf(dialog)).toBeCloseTo(before, 0);
  await widthHandle(dialog).focus();
  await page.keyboard.press('ArrowLeft');
  await expect.poll(() => widthOf(dialog)).toBeCloseTo(before - 40, 0);
  expect(errors.uncaught).toEqual([]);
});

test('a narrow dialog puts the preview under the editor, and a tall one gives the preview the height', async ({
  page,
  errors,
}) => {
  const dialog = await openTemplate(page, 'ymock-jsm');
  const tracks = () =>
    dialog.locator('.tpl-layout').evaluate((el) => getComputedStyle(el).gridTemplateColumns.split(' ').length);
  expect(await tracks()).toBe(2);
  // The window stays 1280 wide: what folds the columns is the dialog's own width.
  await dragBy(page, widthHandle(dialog), -160, 0);
  await expect.poll(tracks).toBe(1);

  const cap = () =>
    dialog.locator('.tpl-card-body').evaluate((el) => parseFloat(getComputedStyle(el).maxHeight));
  expect(await cap()).toBe(220);
  // Dragged far past what the window allows, so the press ends on the backdrop: it does not close.
  await dragBy(page, heightHandle(dialog), 0, 300);
  await expect(dialog).toBeVisible();
  const taller = await dialog.boundingBox();
  await page.setViewportSize({ width: 1280, height: 1200 });
  await dragBy(page, heightHandle(dialog), 0, 150);
  expect((await dialog.boundingBox())?.height ?? 0).toBeGreaterThan(taller?.height ?? 0);
  await expect.poll(cap).toBeGreaterThan(220);
  expect(errors.uncaught).toEqual([]);
});

test('long lines can stay unwrapped, and the choice is remembered', async ({ page, errors }) => {
  let dialog = await openTemplate(page, 'ymock-jsm');
  const wrap = dialog.getByRole('checkbox', { name: 'Wrap long lines' });
  await expect(wrap).toBeChecked();
  await wrap.uncheck();
  await dialog.getByRole('button', { name: 'Edit a copy of this text' }).click();
  await expect(dialog.locator('#tpl-body')).toHaveAttribute('wrap', 'off');
  await expect(dialog.locator('.tpl-card-body').first()).toHaveCSS('white-space', 'pre');

  await dialog.getByRole('button', { name: 'Cancel' }).click();
  dialog = await reopenTemplate(page, 'ymock-jsm');
  await expect(dialog.getByRole('checkbox', { name: 'Wrap long lines' })).not.toBeChecked();
  expect(errors.uncaught).toEqual([]);
});

test('on a phone the dialog is a sheet with no resize handles', async ({ page, errors }) => {
  // `tests/support/app.ts` seeds `uiMode: 'desktop'`, which pins the desktop shell however narrow
  // the window is; this later init script follows the window instead (as `columnResize.spec.ts`).
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode: 'auto' }, version: 0 }),
    );
  });
  const dialog = await openTemplate(page, 'ymock-jsm');
  await expect(widthHandle(dialog)).toHaveCount(1);
  await page.setViewportSize({ width: 390, height: 780 });
  await expect(page.locator('html')).toHaveAttribute('data-viewport', 'mobile');
  await expect(dialog.getByRole('slider')).toHaveCount(0);
  expect(errors.uncaught).toEqual([]);
});
