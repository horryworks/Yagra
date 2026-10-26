// SPDX-License-Identifier: AGPL-3.0-only
// The Japanese text reaches the screen (ADR-214 decision 2).
//
// English ships in the main chunk; every other language is fetched on demand through
// `src/i18nBackend.ts`. The walk seeds `language: 'en'`, so without this spec nothing in a browser
// ever asks that backend for anything — a broken loader would leave a Japanese-speaking operator
// on English with no test failing (`useSuspense: false` renders the fallback rather than an error).

import { expect, test } from '../support/app';

test('a Japanese operator sees the shell in Japanese', async ({ page }) => {
  // Runs after the fixture's own init script, so this preference wins.
  await page.addInitScript(() => {
    localStorage.setItem(
      'yagra_prefs',
      JSON.stringify({
        state: { theme: 'dark', language: 'ja', uiMode: 'desktop', filterRowOpen: true },
        version: 0,
      }),
    );
  });
  await page.goto('/dashboard');
  const nav = page.getByRole('navigation').first();
  // Two labels from the `nav` namespace, which only the lazy backend can supply in Japanese.
  await expect(nav).toContainText('ダッシュボード');
  await expect(nav).toContainText('ノード');
  await expect(nav).not.toContainText('Dashboard');
});
