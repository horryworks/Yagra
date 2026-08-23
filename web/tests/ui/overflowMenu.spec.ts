// SPDX-License-Identifier: AGPL-3.0-only
// The ⋮ row menu opens where it can be seen — on a phone, where it is the only way to act on a row
// (ADR-088 Inc.3).
//
// WHY THIS FILE EXISTS. `OverflowMenu` was one of the three popovers that predate
// `AnchoredPopover`, and the one with the widest reach: twelve list screens collapse their row
// actions into it. It positioned itself with `position: absolute; top: calc(100% + 4px); right: 0`
// — always downward, never measured, never clamped — and on the first run of this file **ten of the
// twelve were broken**: nine clipped away entirely inside `.dt-card-v { overflow: hidden }`, one
// opening 123px off the left edge of a 390px screen. It is now an `ActionMenu`, which portals and
// clamps; this file is what proves that, and what stops the next popover regressing there.
//
// Nothing was watching before, for a structural reason: the menu **only exists on a phone**
// (`useViewportMode() === 'mobile'`; on a desktop the same component renders a bare icon row). The
// route walk runs at 1280px, so it had never once evaluated this surface. `rowMenu.spec.ts` covers
// the *other* hand-rolled menu, on one screen, at desktop width.
//
// The last row is the subject on purpose. A menu that opens downward is fine everywhere except at
// the bottom of the list, which is exactly the place a test that takes `.first()` never looks.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '../support/app';

/** Every screen that collapses its row actions into the ⋮ menu, and the route to reach it.
 *
 *  🚨 **Not hand-maintained — pinned to the imports by the last test in this file.** A thirteenth
 *  screen that adopts `OverflowMenu` fails that test until it is listed here or excused below, so
 *  the cost of the check spreading is one line rather than a silent gap. This is the shape
 *  `filterSpecRegistry.test.ts` and `mcp/tool_source.rs` use for the same reason. */
const SCREENS: Record<string, string> = {
  'pages/ApiTokensPage.tsx': '/settings/api-tokens',
  'pages/AuthSettingsPage.tsx': '/settings/auth',
  'pages/ClassificationRulesPage.tsx': '/nodes/classification-rules',
  'pages/CredentialsPage.tsx': '/nodes/credentials',
  'pages/EventRulesPage.tsx': '/alerts/event-rules',
  'pages/EventSourcesPage.tsx': '/events/webhooks',
  'pages/ForwardingPage.tsx': '/events/forwarding',
  'pages/MaintenancePage.tsx': '/alerts/maintenance',
  'pages/ProfilesPage.tsx': '/nodes/profiles',
  'pages/RoutingPage.tsx': '/alerts/routing',
  'pages/UsersPage.tsx': '/settings/users',
  'troubleshoot/ScheduledPage.tsx': '/troubleshoot/scheduled',
};

/** Consumers that are not a route of their own. Each needs its reason, not just its name — when one
 *  of these becomes reachable by a path, the reason is what tells the next person the exemption has
 *  expired (the shape `NOTE_EXEMPT` uses in `screens.ts`). */
const EXEMPT: Record<string, string> = {
  'components/NodeDetail/CheckConfigActions.tsx':
    'A node-detail tab, not a route. Reaching it needs a node of the right kind and a tab click; the geometry it would exercise is the same menu these twelve already do.',
  'components/shell/UserMenu.tsx':
    'The shell account menu — anchored to the header, not to a row, so it can never be at the bottom of a list. It has its own spec (`userMenu.spec.ts`).',
  'components/ui/ActionMenu.tsx':
    'A wrapper, not a screen: it chooses between the popover and the icon row. Its own popover geometry is covered by `rowMenu.spec.ts` on the node tree.',
  'components/ui/CredentialPicker.tsx':
    'Opens inside a modal, which is centred and scrollable — a different containing block from a table row, and one that cannot push the panel past the viewport edge.',
};

test.describe('the ⋮ row menu on a phone', () => {
  // Below `MOBILE_BP` (768, `lib/viewport.ts`).
  //
  // ⚠️ **The viewport alone is not enough**, and `filterGeometry.spec.ts` paid a run to find out:
  // the shared fixture seeds `uiMode: 'desktop'`, which `resolveViewportMode` honours over the
  // width. Without this init script the app stays in desktop layout at 390px, `OverflowMenu`
  // renders its icon row, and every test here passes while testing nothing.
  test.use({ viewport: { width: 390, height: 844 } });

  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem(
        'yagra_prefs',
        JSON.stringify({
          state: { theme: 'dark', language: 'en', uiMode: 'auto' },
          version: 0,
        }),
      );
    });
  });

  for (const [file, path] of Object.entries(SCREENS)) {
    test(`${path} opens its last row's menu inside the viewport`, async ({ page }) => {
      await page.goto(path);
      await expect(page.locator('.app-loading')).toHaveCount(0, { timeout: 15_000 });

      const triggers = page.locator('.amenu button[aria-haspopup="menu"]');
      // Both halves matter. Zero triggers on a screen listed above means the row actions did not
      // collapse — either the screen rendered no rows under the mock, or it kept the desktop icon
      // row on a phone, and the second is a defect this file would otherwise report as "passed".
      //
      // ⚠️ `expect(locator)`, not `expect(await locator.count())`. The bare count reads once and
      // does not retry, and `.app-loading` clearing is not the same instant as the list committing
      // its rows — the first run of this file reported "no ⋮ trigger" on a screen that had one.
      await expect(
        triggers.first(),
        `${path}: no ⋮ trigger — ${file} imports OverflowMenu but none rendered`,
      ).toBeAttached({ timeout: 10_000 });
      const count = await triggers.count();

      const trigger = triggers.nth(count - 1);
      await trigger.click();

      const menu = page.locator('.apop[role="menu"]');
      await expect(menu, `${path}: the menu did not open`).toHaveCount(1);

      const box = await menu.evaluate((el) => {
        const b = el.getBoundingClientRect();
        // The centre-point probe, as `filterSurface.ts` does it: covered, clipped by an
        // `overflow: hidden` ancestor, or laid out under something else all read the same from the
        // DOM and differ only here.
        const cx = b.left + b.width / 2;
        const cy = b.top + Math.min(b.height / 2, 20);
        const hit = document.elementFromPoint(cx, cy);
        return {
          left: b.left,
          right: b.right,
          top: b.top,
          bottom: b.bottom,
          width: b.width,
          height: b.height,
          vw: window.innerWidth,
          vh: window.innerHeight,
          reached: !!hit && (el === hit || el.contains(hit)),
          onTop: hit ? `<${hit.tagName.toLowerCase()} class="${hit.className}">` : 'null',
        };
      });

      expect(box.width > 0 && box.height > 0, `${path}: the menu has no box`).toBe(true);
      expect(box.left, `${path}: the menu is off the left edge`).toBeGreaterThanOrEqual(0);
      expect(box.right, `${path}: the menu overflows the right edge`).toBeLessThanOrEqual(box.vw);
      // Vertical: the top is what the operator's eye needs. A tall menu whose bottom sits past the
      // fold on a scrollable page is a nuisance; a menu whose *top* is off-screen is nothing at all.
      expect(box.top, `${path}: the menu starts below the fold (${box.top} of ${box.vh})`)
        .toBeLessThan(box.vh);
      expect(
        box.reached,
        `${path}: nothing reaches the menu at its own top — ${box.onTop} is on top, so it is clipped or covered`,
      ).toBe(true);

      // The original symptom of the one popover bug this app has already shipped, and an
      // independent second witness that the panel landed somewhere real.
      expect(
        await page.evaluate(
          () => document.documentElement.scrollWidth > document.documentElement.clientWidth,
        ),
        `${path}: the page scrolls horizontally with the menu open`,
      ).toBe(false);
    });
  }
});

test('every screen that adopts the ⋮ menu is listed above', () => {
  const src = join(process.cwd(), 'src');
  const walk = (dir: string, out: string[] = []): string[] => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) walk(p, out);
      else if (name.endsWith('.tsx')) out.push(p);
    }
    return out;
  };
  const consumers = walk(src)
    .filter((f) => readFileSync(f, 'utf8').includes('OverflowMenu'))
    .map((f) => f.slice(src.length + 1).replace(/\\/g, '/'))
    .filter((f) => f !== 'components/ui/OverflowMenu.tsx');

  // The floor, for the same reason every other check in ADR-088 has one: if the needle stops
  // matching, this test's set is empty and it agrees with itself about nothing.
  expect(
    consumers.length,
    'no file imports OverflowMenu — this test can no longer see the thing it registers',
  ).toBeGreaterThanOrEqual(12);

  const unlisted = consumers.filter((f) => !SCREENS[f] && !EXEMPT[f]);
  expect(
    unlisted,
    'these files use the ⋮ menu but are neither walked nor excused — add a route to SCREENS, or a reason to EXEMPT',
  ).toEqual([]);

  // The other direction: a listed file that no longer uses the menu is a test walking a screen for
  // a reason that has expired, which is how a suite fills up with tests nobody can explain.
  const stale = [...Object.keys(SCREENS), ...Object.keys(EXEMPT)].filter(
    (f) => !consumers.includes(f),
  );
  expect(stale, 'these files are listed here but no longer use the ⋮ menu').toEqual([]);
});
