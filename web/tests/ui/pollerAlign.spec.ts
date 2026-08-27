// SPDX-License-Identifier: AGPL-3.0-only
// The align card's rows show what each site is doing about the upgrade (ADR-051 Inc.4 decision 18).
//
// This exists for a seam nothing else in the repository reaches. The badge's label comes from
// `system:pollers.upgradeStep.*`, a **different i18n namespace** from the rest of the Upgrade page:
// the labels are shared with the Pollers page rather than copied, so there is one set of strings and
// one test (`i18nEnumKeys.test.ts`) pinning them. What that test cannot see is whether the key this
// page builds actually resolves — a `t()` key is a string, so a mistyped namespace prefix compiles,
// passes EN/JA parity (it is missing from both locales), passes Vitest (which never executes a
// `.tsx`), and reaches the operator as the raw key. Verified by mistyping `system:` here: both
// assertions below fail. (Reaching across namespaces without declaring one is the house style —
// 434 call sites do it, because `i18n.ts` inits with `ns: NAMESPACES`.)
//
// What it asserts, and the last one is the reason the file exists. Each row says what its **own**
// site is doing — pulling, silent, or stuck — because a check that only ever sees populated rows
// cannot tell a correct per-row join from one handing every row the same report. The badge resolves
// in English. And it resolves in **Japanese**, which is where a raw key hides: an English word among
// Japanese ones reads as a missing translation rather than as a key that resolves nowhere.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

/** Three sites off this core's build: one mid-pull, one that has never reported, one whose site
 *  updater gave up.
 *
 *  Patched onto the generated body rather than hand-written, so a change to `UpgradeStatusResponse`
 *  reaches this fixture through the contract instead of leaving it describing a shape the API no
 *  longer returns (ADR-052 決定 2). */
function upgradeStatus(): Json {
  const body = defaultBodyFor('/api/v1/system/upgrade') as {
    current: { core_version: string };
    poller_alignment: { pollers: unknown[]; dark_pools: string[]; downgrades: string[] };
  };
  body.current.core_version = '9.9.9';
  body.poller_alignment = {
    pollers: [
      {
        id: 'site-pulling',
        version: '9.9.8',
        progress: {
          command: 'prefetch',
          state: 'running',
          step: 'pull',
          message: 'pulling the image',
        },
      },
      { id: 'site-quiet', version: '9.9.8', progress: null },
      {
        id: 'site-stuck',
        version: '9.9.8',
        progress: {
          command: 'apply',
          state: 'failed',
          step: 'compose',
          message: 'the site updater could not recreate the container',
        },
      },
    ],
    dark_pools: [],
    downgrades: [],
  };
  return body as unknown as Json;
}

test.use({
  viewport: { width: 1440, height: 900 },
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/system/upgrade': () => upgradeStatus() },
  },
});

/** The rows of the align card, in order. Located by a poller id inside the card, so the release
 *  list further down the same page cannot be mistaken for it. */
function alignRows(page: import('@playwright/test').Page) {
  return page
    .locator('.card', { has: page.locator('li:has-text("site-pulling")') })
    .locator('li.upgrade-release');
}

test('each row says what its own site is doing, and the silent one says nothing', async ({
  page,
}) => {
  await page.goto('/settings/upgrade');
  const rows = alignRows(page);
  await expect(rows).toHaveCount(3);

  // Guard the guard: if `.badge` stops matching, an empty list must not read as "no raw keys".
  const pulling = await rows.nth(0).locator('.badge').allTextContents();
  expect(pulling.length, 'the pulling row must carry exactly one badge').toBe(1);
  expect(pulling[0].trim()).toBe('fetching');

  expect(
    await rows.nth(1).locator('.badge').count(),
    'a site with no updater must stay blank, not inherit its neighbour’s report',
  ).toBe(0);

  // A stuck site is the one an operator has to act on, so it is named rather than left blank: the
  // pool's queue has stopped, and nothing else on this page would say so.
  const stuck = rows.nth(2).locator('.badge');
  await expect(stuck).toHaveText('stuck at compose');
  await expect(stuck).toHaveClass(/badge-critical/);
  // Site-authored text, carried as a tooltip and never as a key.
  await expect(stuck).toHaveAttribute(
    'title',
    'the site updater could not recreate the container',
  );
});

test('the badge reads as Japanese in Japanese', async ({ page }) => {
  // Japanese is where a broken key hides. The rest of the screen turns Japanese around it, so an
  // English word — or the raw key itself — reads as one untranslated string rather than as a key
  // that resolves nowhere, and that is what sends someone to "fix" it by duplicating the three
  // labels into this page's own namespace.
  await page.addInitScript(() => {
    window.localStorage.setItem('yagra_prefs', JSON.stringify({ state: { language: 'ja' } }));
  });
  await page.goto('/settings/upgrade');

  const badge = alignRows(page).nth(0).locator('.badge');
  await expect(badge).toHaveText('取得中');
});
