// SPDX-License-Identifier: AGPL-3.0-only
// The resizable Interfaces chart dock (issue #65), in a real layout engine.
//
// This change is *entirely* layout, which is the one category nothing else in the repo can see:
// tsc does not read CSS, and Vitest runs in `environment: 'node'` with no layout engine at all.
// `interfaceDockHeight.test.ts` proves the arithmetic; only this file can prove the arithmetic is
// connected to anything.
//
// The decisive assertion is the first one. The charts run in MetricChart's `fill` mode, which needs
// a *definite* height at every link of a four-step chain (dock height → `.nd-if-dock-charts`
// `flex: 1` + `grid-auto-rows: minmax(0, 1fr)` → `.nd-if-chart` `min-height: 0` →
// `.metricchart-fill` `flex: 1`). Break any one and the chart does not error — it collapses to
// uPlot's `MIN_PLOT_HEIGHT` of 40px, which is *smaller* than the 132px this change exists to
// enlarge. Silent, and the exact inverse of the fix.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** The dock only exists on a node that has interfaces, and only a `device` shows the tab. */
function deviceNode(): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as { kind: string };
  body.kind = 'device';
  return body as unknown as Json;
}

/** What the charts were before this change — the bar the fill chain must clear. */
const CHART_HEIGHT_BEFORE = 132;

// ⚠️ Deliberately taller than Playwright's 720p Desktop Chrome default. At 720p the tab body is
// ~488px, the clamp lands on `DOCK_MIN_PX`, and a drag has ~68px of travel — which makes a drag
// assertion true but uninformative. This is a test about the mechanism, not about a small laptop.
test.use({
  viewport: { width: 1280, height: 1000 },
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => deviceNode() },
  },
});

/** Open the Interfaces tab and select the first interface, which is what mounts the dock. */
async function openDock(page: import('@playwright/test').Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await page.locator('.nd-if-row').first().click();
  await expect(page.locator('.nd-if-dock')).toBeVisible({ timeout: 15_000 });
}

async function heightOf(page: import('@playwright/test').Page, selector: string) {
  const box = await page.locator(selector).first().boundingBox();
  expect(box, `${selector} has no box`).not.toBeNull();
  return box!.height;
}

test('the chart fills the dock instead of collapsing to the uPlot floor', async ({ page }) => {
  await openDock(page);
  const dock = await heightOf(page, '.nd-if-dock');
  const chart = await heightOf(page, '.nd-if-chart > .metricchart-fill');

  // Both halves matter. "A large fraction of the dock" catches a broken link in the fill chain
  // (the chart would be 40px in a 400px dock); "taller than it used to be" catches the case where
  // the chain works but the dock itself never got its new default, which would leave the issue
  // unfixed while every structural assertion passed.
  expect(chart, 'chart collapsed — a link in the fill chain is missing').toBeGreaterThan(dock * 0.4);
  expect(chart, 'no taller than before this change').toBeGreaterThan(CHART_HEIGHT_BEFORE);
});

test('dragging the top edge upward makes the dock and its charts taller', async ({ page }) => {
  await openDock(page);
  const before = await heightOf(page, '.nd-if-dock');
  const chartBefore = await heightOf(page, '.nd-if-chart > .metricchart-fill');

  const handle = page.getByRole('slider', { name: /resize/i });
  const box = (await handle.boundingBox())!;
  const midX = box.x + box.width / 2;
  const midY = box.y + box.height / 2;

  // ⚠️ This is where the inverted sign is proven against the real DOM. The handle sits ABOVE the
  // dock, so moving the pointer UP must GROW it — the opposite of the Geo map's handle, and the
  // thing a copy-paste from `mapPaneHeight.ts` gets wrong while every unit test still passes.
  await page.mouse.move(midX, midY);
  await page.mouse.down();
  await page.mouse.move(midX, midY - 120, { steps: 8 });
  await page.mouse.up();

  expect(await heightOf(page, '.nd-if-dock')).toBeGreaterThan(before + 80);
  expect(await heightOf(page, '.nd-if-chart > .metricchart-fill')).toBeGreaterThan(chartBefore + 80);
});

test('the interface list cannot be dragged out of existence', async ({ page }) => {
  await openDock(page);
  const handle = page.getByRole('slider', { name: /resize/i });
  const box = (await handle.boundingBox())!;

  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, -2000, { steps: 10 });
  await page.mouse.up();

  // `LIST_MIN_PX` is 160. Asserting a little under it keeps the test about "the floor holds"
  // rather than about the exact constant, which `interfaceDockHeight.test.ts` already pins.
  expect(await heightOf(page, '.nd-if-list')).toBeGreaterThan(140);
});

test('resizing logs nothing — the ResizeObserver chain does not oscillate', async ({
  page,
  errors,
}) => {
  await openDock(page);
  const handle = page.getByRole('slider', { name: /resize/i });
  const box = (await handle.boundingBox())!;
  const midX = box.x + box.width / 2;

  // Drag both ways: the fill-mode oscillation the MetricChart header warns about surfaces as
  // "ResizeObserver loop completed with undelivered notifications" on the console, not as a throw.
  await page.mouse.move(midX, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(midX, box.y - 150, { steps: 10 });
  await page.mouse.move(midX, box.y + 100, { steps: 10 });
  await page.mouse.up();

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

// Not asserted, and worth saying why: **that the list does not scroll during a drag**. The generated
// mock answers `/interfaces` with exactly one row, so `.nd-if-list` has nothing to scroll and the
// defect the drag-suppression flag exists to prevent cannot be reproduced here. Revisit if the
// fixture ever grows past one interface.
