// SPDX-License-Identifier: AGPL-3.0-only
// A site that has not said an upgrade is safe there is named before the press (ADR-051 Inc.7).
//
// 🚨 This is the only automated reader of that warning. Vitest cannot be it — `upgradeStatus.ts`'s
// unit tests cover `unpreparedSites`, and the wiring from the server's `needs_site_prep` to a
// coloured line on the screen lives in a `.tsx`, which Vitest does not run at all (testing.md). The
// route walk visits this screen but only asks that it rendered *something*, so a field renamed on
// the Rust side would take the warning off the page with 2,500 tests and 200 walk steps green.
//
// What is being protected is not a string: it is that pressing UPGRADE on a site whose updater
// predates the fix takes that site off the bus, silently, with both ends reporting success and
// nobody able to repair it without hands at the site.

import { expect, test } from '../support/app';

/** The mocked fleet carries exactly one unprepared site (`tests/support/bootstrap.ts`). */
const SITE = 'edge-tokyo-1';

test('the components card names the site that has not said an upgrade is safe there', async ({
  page,
}) => {
  await page.goto('/settings/upgrade');
  const row = page.locator('.upgrade-components li', { hasText: SITE });
  await expect(row).toHaveCount(1);

  // 🚨 Read the warning through its own class, not through the row's text. A `hasText` on the row
  // passes on any of its six cells, so it cannot tell "the warning is there" from "the version is
  // there" — the trap `widget-catalog-is-tier1-testable` records.
  const warn = row.locator('.upgrade-why-warn');
  await expect(warn).toHaveCount(1);
  expect((await warn.innerText()).trim().length).toBeGreaterThan(0);

  // Coloured with the domain's own warning token, not merely "not muted": deleting the rule makes
  // the span inherit the body colour, which is already different from muted, so a not-equal
  // assertion would pass over exactly the change it exists to catch. The token is resolved through
  // a probe rather than compared as a string — `color` computes to `rgb(...)` and the token is a
  // hex, and this way the check follows the theme instead of pinning one palette.
  const colour = await warn.evaluate((el) => {
    const probe = document.createElement('span');
    probe.style.color = 'var(--status-warning)';
    el.parentElement?.appendChild(probe);
    const want = getComputedStyle(probe).color;
    probe.remove();
    return { own: getComputedStyle(el).color, want };
  });
  expect(colour.own).toBe(colour.want);

  // The rows that cannot take a site off the air must not carry it. core is the one the generated
  // body always provides, and it is this deployment's own host.
  const core = page.locator('.upgrade-components li').first();
  await expect(core.locator('.upgrade-why-warn')).toHaveCount(0);
});
