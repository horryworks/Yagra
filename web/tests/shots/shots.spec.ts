// SPDX-License-Identifier: AGPL-3.0-only
// ADR-246: a picture of every screen, compared with the picture taken last time.
//
// WHY IT EXISTS. ADR-246 moves every screen onto the shared layout parts and snaps about 460 raw
// spacing values onto the scale. Tier1 proves each screen still renders its data and that nothing
// is cut off or laid out off the page; it cannot say what the screen now LOOKS like. This can:
// `toHaveScreenshot` paints every changed pixel in a diff image under `test-results/`.
//
// HOW TO USE IT (it is not part of `npm run test:ui`, and its pictures are never committed):
//   npm run build && npm run test:shots -- --update-snapshots   # before a change: the baseline
//   npm run build && npm run test:shots                         # after it: every screen that moved
//                                                               # fails with *-diff.png beside it
// Read the diff images. A shift of a few px where a value was snapped is the change; text over
// text, a part that vanished or a scrollbar that appeared is a defect.
//
// What it does not see - the same blind spots as Tier1's walk: dialogs, tabs other than the one a
// screen opens on, Japanese, and anything below the fold of an inner scroller. The window is made
// tall to push that fold down, not to remove it.

import { expect, test } from '../support/app';
import { ALL_SCREENS } from '../ui/screens';

/** Dates are drawn relative to now ("3 minutes ago"); a fixed clock keeps two runs comparable. */
const NOW = new Date('2026-11-01T09:00:00Z');

const LAYOUTS = {
  desktop: { width: 1440, height: 1800, uiMode: 'desktop' },
  // Below 768px with `uiMode: 'auto'` is the phone layout (`data-viewport="mobile"`). The fixture
  // seeds `desktop`, which pins the desktop shell however narrow the window, so it is overwritten.
  phone: { width: 390, height: 1600, uiMode: 'auto' },
} as const;

for (const [layout, spec] of Object.entries(LAYOUTS)) {
  for (const screen of ALL_SCREENS) {
    test(`${layout} ${screen.path}`, async ({ page }) => {
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
      await page.goto(`${screen.path}${screen.query ?? ''}`);
      if (layout === 'phone') {
        await expect(page.locator('html')).toHaveAttribute('data-viewport', 'mobile');
      }
      const name = `${layout}${screen.path.replace(/[^a-z0-9]+/gi, '-').replace(/-$/, '') || '-root'}.png`;
      await expect(page).toHaveScreenshot(name, {
        fullPage: true,
        animations: 'disabled',
        caret: 'hide',
        timeout: 20_000,
      });
    });
  }
}
