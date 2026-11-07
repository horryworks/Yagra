// SPDX-License-Identifier: AGPL-3.0-only
// ADR-246 Inc.4: a picture of the dialogs, compared with the picture taken last time.
//
// `shots.spec.ts` pictures every screen as it opens, and a dialog is never open then - its blind
// spot, and Inc.4 moves exactly those (the fields of the add and edit dialogs onto `Fields` /
// `Field`). Each row here opens one dialog the way an operator would and pictures the dialog
// alone. Same use as the screen pictures:
//   npm run build && npm run test:shots -- --update-snapshots   # the baseline, before a change
//   npm run build && npm run test:shots                         # after it
//
// A row is a screen and the button that opens the dialog on it. A dialog behind a menu or a tab
// takes `before`, the clicks that reach the button.

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';

const NOW = new Date('2026-11-01T09:00:00Z');

interface DialogShot {
  name: string;
  path: string;
  /** The button that opens the dialog, by its accessible name (a substring, as Playwright reads it). */
  open: string;
  before?: (page: Page) => Promise<void>;
  /** The phone layout has no inventory tree to select rows in. */
  desktopOnly?: boolean;
}

const DIALOGS: DialogShot[] = [
  { name: 'user-add', path: '/settings/users', open: 'Add user' },
  { name: 'token-add', path: '/settings/api-tokens', open: 'New token' },
  { name: 'forwarding-add', path: '/events/forwarding', open: 'New destination' },
  { name: 'event-source-add', path: '/events/webhooks', open: 'Add source' },
  { name: 'threshold-add', path: '/alerts/rules', open: 'Add rule' },
  { name: 'event-rule-add', path: '/alerts/rules/events', open: 'Add rule' },
  { name: 'routing-channel-add', path: '/alerts/routing', open: 'Add channel' },
  { name: 'routing-rule-add', path: '/alerts/routing', open: 'Add rule' },
  { name: 'maintenance-add', path: '/alerts/maintenance', open: 'Add window' },
  { name: 'pool-add', path: '/settings/pollers', open: 'Create pool' },
  { name: 'profile-add', path: '/nodes/collection', open: 'Add profile' },
  { name: 'classification-rule-add', path: '/nodes/collection/rules', open: 'Add rule' },
  { name: 'mib-add', path: '/nodes/collection/mib', open: 'Add entry' },
  { name: 'metric-set-add', path: '/nodes/collection/metric-sets', open: 'Add metric set' },
  { name: 'report-add', path: '/dashboard/reports?tab=templates', open: 'New report' },
  // The button's accessible name is its aria-label, not the word on it ("Change").
  { name: 'config-schedule', path: '/nodes/config-backups', open: 'Backup schedule' },
  { name: 'node-add', path: '/nodes', open: 'Add node' },
  { name: 'node-edit', path: '/nodes/00000000-0000-4000-8000-000000000001', open: 'Edit node' },
  {
    // The inventory page hides the tree's own toolbar, so a folder's menu is the way in.
    name: 'group-add',
    path: '/nodes',
    open: 'Add subgroup',
    before: async (page) => {
      await page.locator('.ntree-grow').first().click({ button: 'right' });
    },
  },
  {
    name: 'tag-nodes',
    path: '/nodes',
    open: 'Tag…',
    desktopOnly: true,
    before: async (page) => {
      const rows = page.locator('.ntree-node');
      await expect(rows).toHaveCount(3);
      await rows.nth(0).click();
      await rows.nth(2).click({ modifiers: ['Shift'] });
    },
  },
  {
    name: 'change-password',
    path: '/nodes',
    open: 'Change my password',
    before: async (page) => {
      await page.locator('.usermenu-avatar').click();
    },
  },
];

const LAYOUTS = {
  desktop: { width: 1440, height: 1100, uiMode: 'desktop' },
  phone: { width: 390, height: 1000, uiMode: 'auto' },
} as const;

for (const [layout, spec] of Object.entries(LAYOUTS)) {
  for (const d of DIALOGS.filter((x) => layout === 'desktop' || !x.desktopOnly)) {
    test(`${layout} dialog ${d.name}`, async ({ page }) => {
      await page.clock.setFixedTime(NOW);
      await page.setViewportSize({ width: spec.width, height: spec.height });
      if (spec.uiMode !== 'desktop') {
        await page.addInitScript((uiMode) => {
          localStorage.setItem(
            'yagra_prefs',
            JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode }, version: 0 }),
          );
        }, spec.uiMode);
      }
      await page.goto(d.path);
      if (d.before) await d.before(page);
      await page.getByRole('button', { name: d.open }).first().click();
      const dialog = page.getByRole('dialog');
      await expect(dialog).toBeVisible();
      await expect(dialog).toHaveScreenshot(`${layout}-dialog-${d.name}.png`, {
        animations: 'disabled',
        caret: 'hide',
        timeout: 20_000,
      });
    });
  }
}
