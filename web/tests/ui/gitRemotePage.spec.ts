// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations ▸ Git (ADR-210 stage 4).
//
// WHY A BROWSER. What the form sends — a token only when one is typed, nothing secret for a key —
// and that a failing push and the deploy key reach the screen, are wired in `GitRemotePage.tsx`,
// which Vitest never runs; the route walk does not visit a page under Integrations.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

type Schemas = components['schemas'];

const none = {
  configured: false,
  url: null,
  branch: null,
  auth: null,
  username: null,
  ssh_public_key: null,
  host_keys: [],
  status: {
    configured: false,
    host: null,
    pushed_at: null,
    pushed_commit: null,
    failure: null,
    failed_at: null,
    failures: 0,
  },
} satisfies Schemas['RemoteView'];

const failing = {
  configured: true,
  url: 'git@example.com:acme/configs.git',
  branch: 'main',
  auth: 'ssh_key',
  username: null,
  ssh_public_key: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl yagra-config-backup',
  host_keys: ['ssh-ed25519 SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU'],
  status: {
    configured: true,
    host: 'example.com',
    pushed_at: null,
    pushed_commit: null,
    failure: 'auth',
    failed_at: '2026-10-23T06:00:00Z',
    failures: 1,
  },
} satisfies Schemas['RemoteView'];

const overrides = (view: Schemas['RemoteView']) => ({
  ...BOOTSTRAP_OVERRIDES,
  '/api/v1/config-backup-remote': view as unknown as Json,
  '/api/v1/config-backup-remote/status': view.status as unknown as Json,
});

test.describe('no destination yet', () => {
  test.use({ mockConfig: { overrides: overrides(none) } });

  test('a token destination sends the user name and token it was given', async ({
    page,
    errors,
  }) => {
    await page.goto('/settings/integrations/git');
    await page.getByRole('button', { name: 'Set destination' }).click();
    const dialog = page.getByRole('dialog');
    await dialog.getByRole('textbox').first().fill('https://example.com/acme/configs.git');
    await dialog.locator('input[placeholder="x-access-token"]').fill('bot');
    await dialog.locator('#git-token').fill('tok-1');
    await expect(dialog.locator('.form-warning')).toContainText('private, empty repository');
    const put = page.waitForRequest(
      (r) => r.method() === 'PUT' && new URL(r.url()).pathname === '/api/v1/config-backup-remote',
    );
    await dialog.getByRole('button', { name: 'Save' }).click();
    expect((await put).postDataJSON()).toEqual({
      url: 'https://example.com/acme/configs.git',
      branch: null,
      auth: 'https_token',
      username: 'bot',
      token: 'tok-1',
    });
    expect(errors.uncaught).toEqual([]);
  });
});

test.describe('an SSH destination whose push was refused', () => {
  test.use({ mockConfig: { overrides: overrides(failing) } });

  test('the failure, the deploy key and the host key are on the page', async ({ page }) => {
    await page.goto('/settings/integrations/git');
    await expect(page.locator('.git-line.bad')).toContainText('the login was refused');
    await expect(page.locator('.copy-cmd-text')).toContainText('yagra-config-backup');
    await expect(page.locator('.git-facts')).toContainText('SHA256:+DiY3wvvV6');
  });

  test('the Config backups list says where the backups go', async ({ page }) => {
    await page.goto('/nodes/config-backups');
    const line = page.locator('.cfgbl-push.bad');
    await expect(line).toContainText('Push failed');
    await expect(line.getByRole('link')).toHaveAttribute('href', '/settings/integrations/git');
  });
});
