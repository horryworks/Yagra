// SPDX-License-Identifier: AGPL-3.0-only
// ADR-053 in a browser: the half of the filter row that is not a predicate.
//
// `columnFilter.spec.ts` covers what the controls *do* — reaching the request, the URL round-trip,
// clear-all. This file covers whether they are **there**, which is the half that shipped broken six
// times and that 1,966 unit tests could not ask about. Three properties, each browser-only:
//
//   1. **The window is not always 1280px.** The filter row shares one `gridTemplateColumns` with
//      the header and every data row, and a track that resolves differently in one of the three
//      slides every control out from under its column. Whether it does depends on the width the
//      operator's window happens to be.
//   2. **The popover has to escape the table.** `.dt` is `overflow: hidden` and a virtualized row
//      carries a `transform`, which is a containing block for `position: fixed` — so the panel is
//      portalled to `<body>`. A regression here does not throw; the panel is laid out somewhere
//      off-screen and the filter simply cannot be opened.
//   3. **Mobile is an either/or, not a variant.** `FilterBar` and `DataTable` both refuse to draw a
//      filter row under `useViewportMode() === 'mobile'`, and `MobileFilterButton` refuses to draw
//      unless. A screen that lost the second half still looks correct — it has simply stopped
//      being filterable on a phone, silently.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { FILTER_SURFACE, inspectFilterSurface, MUST_FILTER } from './filterSurface';

const SCREENS = Object.keys(MUST_FILTER);

/** The two node-detail tabs that hand-roll a filter row of their own. They are not routes, so the
 *  walk's screen list cannot reach them — and both were drawing their row on a phone. */
const NODE_ID = '00000000-0000-4000-8000-0000000000aa';
const TABS_WITH_A_FILTER_ROW = ['interfaces', 'collection'];

function deviceNode(): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as { kind: string };
  body.kind = 'device';
  return body as unknown as Json;
}

/** Widths an operator actually has. 1280 is Playwright's Desktop Chrome and the walk's default, so
 *  it is deliberately not repeated here; these are the two that bracket it.
 *
 *  ⚠️ The narrow one is not a phone — mobile mode starts below 768 and swaps the whole layout. 1152
 *  is a 15" laptop at 125% scaling, which is an ordinary way to run this UI and is the width at
 *  which a table's fixed columns stop fitting. */
const WIDTHS = [1152, 1920];

for (const width of WIDTHS) {
  test.describe(`at ${width}px`, () => {
    test.use({ viewport: { width, height: 900 } });

    for (const path of SCREENS) {
      test(`${path} keeps its filter controls under their columns`, async ({ page }) => {
        await page.goto(path);
        await expect(page.locator('.dt-f-trigger').first()).toBeVisible();
        expect(await inspectFilterSurface(page), `${path} at ${width}px`).toEqual([]);
      });
    }
  });
}

test.describe('the filter popover', () => {
  // Audit is a server-side filter row on a wide table, so its last column is at the right edge —
  // the side a panel escapes from. The first column exercises the left edge in the same run.
  const PATH = '/settings/audit';

  test('opens outside the table it belongs to, and inside the window', async ({ page }) => {
    await page.goto(PATH);
    const triggers = page.locator('.dt-f-trigger');
    await expect(triggers.first()).toBeVisible();
    const count = await triggers.count();

    for (const index of [0, count - 1]) {
      await triggers.nth(index).click();
      const panel = page.getByRole('dialog');
      await expect(panel).toBeVisible();

      // Portalled, not nested. This is the assertion that fails if someone "simplifies"
      // `AnchoredPopover` back into the table: `.dt` is `overflow: hidden`, so a panel that is
      // still a descendant of the row is clipped at the table's edge.
      const insideTable = await panel.evaluate((el) => el.closest('.dt') !== null);
      expect(insideTable, `the panel for column ${index} is inside the clipped table`).toBe(false);

      const box = await panel.boundingBox();
      const view = page.viewportSize();
      expect(box).not.toBeNull();
      if (!box || !view) return;
      expect(box.x, `column ${index}: the panel starts off the left edge`).toBeGreaterThanOrEqual(
        0,
      );
      expect(
        box.x + box.width,
        `column ${index}: the panel runs off the right edge`,
      ).toBeLessThanOrEqual(view.width);
      expect(
        box.y + box.height,
        `column ${index}: the panel runs off the bottom`,
      ).toBeLessThanOrEqual(view.height);

      await page.keyboard.press('Escape');
      await expect(panel).toHaveCount(0);
    }
  });
});

test.describe('on a phone', () => {
  // Below `MOBILE_BP` (768, `lib/viewport.ts`).
  //
  // ⚠️ The viewport alone is not enough, and finding that out cost a run: the shared fixture seeds
  // `uiMode: 'desktop'`, and `resolveViewportMode` returns 'desktop' unconditionally for it — a
  // width override that a *user preference* outranks. So at 390px the app stayed in desktop layout
  // and squeezed its tables to nothing, which failed as "no rows" rather than as "still desktop".
  // This init script runs after the fixture's, so it wins.
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

  /** The either/or, asserted in both directions. `where` names the screen for the failure text. */
  async function expectTheSheetInsteadOfTheRow(
    page: import('@playwright/test').Page,
    where: string,
  ) {
    // Half one: the desktop surfaces are gone. Each refuses in its own component, off the same
    // `useViewportMode()`, and the bug this catches shipped twice — first as the row and a
    // `Filter (N)` button on a desktop, then as both of them on a phone.
    expect(
      await page.locator(FILTER_SURFACE).count(),
      `${where}: a desktop filter row/bar on a phone`,
    ).toBe(0);

    // Half two: something replaced them. A screen that is filterable on a desktop and not on a
    // phone is not a layout choice, it is a lost feature — and it looks completely normal.
    expect(
      await page.locator('.mfilt-btn').count(),
      `${where}: filterable on a desktop, not filterable here`,
    ).toBeGreaterThan(0);
  }

  for (const path of SCREENS) {
    test(`${path} offers the sheet instead of the row`, async ({ page }) => {
      await page.goto(path);
      await expect(page.locator('.app-loading')).toHaveCount(0, {
        timeout: 15_000,
      });
      await expect(page.locator('.dt-row, .fbar, .mfilt-btn, .ntree-row').first()).toBeVisible({
        timeout: 15_000,
      });
      await expectTheSheetInsteadOfTheRow(page, path);
    });
  }

  // A node-detail tab is a screen with a filter row that no route names, so nothing above visits
  // it. `Interfaces` is where the defect was reported from: its row is `position: sticky` at the
  // height of a header that mobile hides, so the rows scrolled through the gap above it.
  test.describe('a node detail tab', () => {
    test.use({
      mockConfig: {
        overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => deviceNode() },
      },
    });

    for (const tab of TABS_WITH_A_FILTER_ROW) {
      test(`${tab} offers the sheet instead of the row`, async ({ page }) => {
        await page.goto(`/nodes/${NODE_ID}?tab=${tab}`);
        await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
        await expect(page.locator('.mfilt-btn').first()).toBeVisible({ timeout: 15_000 });
        await expectTheSheetInsteadOfTheRow(page, `?tab=${tab}`);
      });
    }
  });
});
