// SPDX-License-Identifier: AGPL-3.0-only
// The filter row's default and its toggle (ADR-053 Inc.9), in a browser.
//
// WHY IT IS ITS OWN FILE. The shared fixture seeds `filterRowOpen: true`, because every other
// filter spec is about what the row *contains*. That makes this the one place where the shipped
// default is exercised, so it seeds the pref closed and asserts **both** directions: a change that
// draws the row always, and a change that never draws it, each pass the other file's tests
// unchanged. One-directional coverage here would be worth very little.
//
// The lock is the third property and the least obvious: while a filter is narrowing the list, the
// button may not hide the row. That is not a UI preference — a list with rows missing and no
// visible control responsible for the narrowing is the failure `columnFilter.ts` argues against at
// `EnumFilterSpec.single`'s deletion.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { FILTER_SURFACE } from './filterSurface';

/** Two screens, deliberately: `/nodes/mib` is a `DataTable` filter row and `/alerts` is a
 *  `FilterBar` over a list with no header. They are different components reading one decision, and
 *  the decision having drifted between them is exactly the bug Inc.9's predecessor shipped. */
const SCREENS = ['/nodes/mib', '/alerts'];

/** The shipped default: closed. The fixture's own init script runs first, so this overwrites it. */
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({
        state: { theme: 'dark', language: 'en', uiMode: 'desktop', filterRowOpen: false },
        version: 0,
      }),
    );
  });
});

const toggle = (page: import('@playwright/test').Page) => page.locator('.mfilt-btn').first();
const surfaces = (page: import('@playwright/test').Page) => page.locator(FILTER_SURFACE);

for (const path of SCREENS) {
  test(`${path} starts with the filter row closed, and the button opens it`, async ({ page }) => {
    await page.goto(path);
    // Wait for the screen rather than for the row: asserting a count of 0 on a page that has not
    // finished rendering passes for the wrong reason, which is the whole shape of this bug class.
    await expect(toggle(page)).toBeVisible({ timeout: 15_000 });

    expect(await surfaces(page).count(), `${path}: the row is drawn before anyone asked`).toBe(0);
    await expect(toggle(page), `${path}: the toggle does not report a closed row`).toHaveAttribute(
      'aria-pressed',
      'false',
    );

    await toggle(page).click();
    await expect(surfaces(page).first(), `${path}: the button did not open the row`).toBeVisible();
    await expect(toggle(page)).toHaveAttribute('aria-pressed', 'true');

    // The press has to reach the persisted store, or the choice dies with the tab. Read the pref
    // rather than reloading: `addInitScript` re-seeds `yagra_prefs` on *every* navigation in this
    // file, so a reload here would assert the fixture rather than the feature.
    const stored = await page.evaluate(() => localStorage.getItem('yagra_prefs') ?? '');
    expect(JSON.parse(stored).state.filterRowOpen, `${path}: the press was not persisted`).toBe(
      true,
    );

    await toggle(page).click();
    expect(await surfaces(page).count(), `${path}: the button did not close the row`).toBe(0);
  });
}

test.describe('with the preference set', () => {
  // One boolean, every screen. A per-screen memory would pass every test above and still fail here.
  // Seeded rather than clicked: the `beforeEach` init script runs again on each navigation, so a
  // click on one screen cannot survive a `goto` to the next in this file.
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem(
        'yagra_prefs',
        JSON.stringify({
          state: { theme: 'dark', language: 'en', uiMode: 'desktop', filterRowOpen: true },
          version: 0,
        }),
      );
    });
  });

  test('every screen draws its row off the one setting', async ({ page }) => {
    for (const path of SCREENS) {
      await page.goto(path);
      await expect(surfaces(page).first(), `${path}: the row ignored the preference`).toBeVisible({
        timeout: 15_000,
      });
    }
  });
});

test.describe('while a filter is narrowing the list', () => {
  test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

  test('the row is forced open and the toggle refuses to close it', async ({ page }) => {
    // Events, because its filters live in the URL — which is how the row gets forced open for
    // someone who was *sent* a link rather than having set the filter themselves.
    await page.goto('/events');
    await expect(toggle(page)).toBeVisible({ timeout: 15_000 });
    await toggle(page).click();

    await page.getByRole('button', { name: /Filter by Message/ }).click();
    await page.getByRole('dialog').getByRole('searchbox').first().fill('router');
    // ⚠️ The URL first, `Escape` second. The text condition is debounced, and closing the popover
    // before it commits leaves the term nowhere — which reads here as "the filter never applied".
    await expect(page).toHaveURL(/router/);
    await page.keyboard.press('Escape');

    await expect(toggle(page), 'a narrowing list left its toggle closable').toHaveAttribute(
      'aria-disabled',
      'true',
    );
    await toggle(page).click({ force: true });
    await expect(surfaces(page).first(), 'the row hid the filter that is narrowing').toBeVisible();

    // Reload from the URL the screen wrote, with the preference still closed: arriving on a shared
    // link must show the controls responsible for the missing rows. This is the case the whole
    // decision exists for, and no amount of clicking in one session demonstrates it.
    await page.goto(page.url());
    await expect(surfaces(page).first(), 'a shared filtered link hid its own filter').toBeVisible({
      timeout: 15_000,
    });

    // And the way out is the button that is guaranteed to be beside it.
    await page.getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page).not.toHaveURL(/router/);
    expect(await surfaces(page).count(), 'clearing left the row forced open').toBe(0);
  });
});
