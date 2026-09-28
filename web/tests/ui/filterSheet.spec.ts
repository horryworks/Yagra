// SPDX-License-Identifier: AGPL-3.0-only
// The filter sheet on a phone, used rather than looked at (ADR-053 decision 14, ADR-184).
//
// `filterGeometry.spec.ts` proves a phone is offered the sheet *instead of* the row. It stops at the
// button. This file presses it: the sheet has to cover the screen, name each column in words, get
// a change as far as the URL, and give focus back to the button when it closes.
//
// Written before the toolbars moved onto the shared one (ADR-184 increment 23), because that move
// changes where the sheet is mounted — from after the table to inside the toolbar's own component —
// and a sheet that opened behind the table, or lost its labels, would still look like a button that
// works.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** A route, or a node-detail tab (a filter row no route names). */
const SUBJECTS = [
  '/alerts/mutes',
  '/events',
  '/settings/users',
  `/nodes/${NODE_ID}?tab=interfaces`,
  `/nodes/${NODE_ID}?tab=flow`,
  `/nodes/${NODE_ID}?tab=collection`,
];

test.use({
  viewport: { width: 390, height: 844 },
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID) },
  },
});

test.beforeEach(async ({ page }) => {
  // The fixture seeds `uiMode: 'desktop'`, which outranks the width; `auto` is what lets 390px mean
  // a phone. Runs after the fixture's init script, so it wins.
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode: 'auto' }, version: 0 }),
    );
  });
});

for (const path of SUBJECTS) {
  test(`${path}: the sheet opens over the screen, edits the URL and gives focus back`, async ({
    page,
  }) => {
    await page.goto(path);
    const button = page.locator('.mfilt-btn').first();
    await expect(button).toBeVisible({ timeout: 15_000 });
    // The layer being measured. Without this the fixture could change and the test would quietly
    // go on pressing a desktop toggle.
    expect(await page.evaluate(() => document.documentElement.getAttribute('data-viewport'))).toBe(
      'mobile',
    );

    await button.click();
    const sheet = page.getByRole('dialog');
    await expect(sheet).toBeVisible();

    // Over the screen, not beside or behind the table: a bottom sheet spans the width.
    const box = await sheet.boundingBox();
    expect(box, 'the sheet did not lay out').not.toBeNull();
    expect(box!.width, 'the sheet does not span the phone').toBeGreaterThanOrEqual(390 - 2);
    const topmost = await sheet.evaluate((el) => {
      const r = el.getBoundingClientRect();
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + Math.min(20, r.height / 2));
      return hit !== null && el.contains(hit);
    });
    expect(topmost, 'something is drawn over the sheet').toBe(true);

    // Each column is named in words — a sheet has room for a real label, and a missing one falls
    // back to the column key.
    const headings = sheet.locator('.mfilt-h');
    expect(await headings.count(), 'the sheet lists no columns').toBeGreaterThan(0);
    for (const text of await headings.allInnerTexts()) {
      const label = text.replace('●', '').trim();
      expect(label, 'a column with no label').not.toBe('');
      expect(label, `"${label}" is a key, not a label`).not.toMatch(/^[a-z_]+$/);
    }

    // A change reaches the URL: pick the first option of the first set-valued column.
    const before = page.url();
    const option = sheet.getByRole('option').first();
    await expect(option).toBeVisible();
    await option.click();
    await expect.poll(() => page.url()).not.toBe(before);

    // Closing hands focus back to the control that opened it.
    await page.keyboard.press('Escape');
    await expect(sheet).toHaveCount(0);
    await expect(page.locator('.mfilt-btn').first()).toBeFocused();
  });
}
