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
import { DOCK_MIN_PX } from '../../src/components/NodeDetail/interfaceDockHeight';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** The dock only exists on a node that has interfaces, and only a `device` shows the tab. */
function deviceNode(): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as { kind: string };
  body.kind = 'device';
  return body as unknown as Json;
}

/** A series covering the window the client actually asked for.
 *
 *  ⚠️ The generated mock gives every array exactly one element, and numbers come out as `1` — so
 *  the only sample sits at Unix second 1, an hour of 1970 outside the requested range. uPlot then
 *  reports no data in range, which makes both the idle legend and any hover unreachable: the
 *  cursor index is permanently null. Reading `from`/`to` off the request rather than inventing a
 *  window keeps this honest if `RangeControl`'s default ever changes. */
function seriesBody(url: URL): Json {
  const from = Number(url.searchParams.get('from'));
  const to = Number(url.searchParams.get('to'));
  const n = 12;
  const timestamps = Array.from({ length: n }, (_, i) =>
    Math.round(from + ((to - from) * i) / (n - 1)),
  );
  // Strictly increasing per series, so "the legend moved" is provable from the text alone.
  const ramp = (base: number) => timestamps.map((_, i) => base * (i + 1));
  return {
    timestamps,
    in_bps: ramp(1_000_000),
    out_bps: ramp(500_000),
    in_ucast_pps: ramp(100),
    out_ucast_pps: ramp(50),
    in_errors: ramp(1),
    out_errors: ramp(2),
    in_discards: ramp(3),
    out_discards: ramp(4),
  } as unknown as Json;
}

/** What the charts were before this change — the bar the fill chain must clear. */
const CHART_HEIGHT_BEFORE = 132;

// ⚠️ Deliberately taller than Playwright's 720p Desktop Chrome default. At 720p the tab body is
// ~488px, the clamp lands on `DOCK_MIN_PX`, and a drag has ~68px of travel — which makes a drag
// assertion true but uninformative. This is a test about the mechanism, not about a small laptop.
test.use({
  viewport: { width: 1280, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(),
      '/api/v1/nodes/{node_id}/interfaces/{ifindex}/series': (url: URL) => seriesBody(url),
    },
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

test('the dock chrome still leaves the floor its promised chart height', async ({ page }) => {
  await openDock(page);
  const dock = await heightOf(page, '.nd-if-dock');
  const plot = await heightOf(page, '.nd-if-chart > .metricchart-fill');
  const chrome = dock - plot;

  // 🚨 This is the check `DOCK_MIN_PX`'s doc asks for and never had. That constant is 260 because
  // the dock's chrome was counted off the CSS at "roughly 120px", so the floor would still leave
  // the charts the 132px they had before the dock shipped. Nothing measured it, and the note there
  // said so: if the chrome grows, 260 quietly stops meaning what its name says, and the failure is
  // a chart that is *smaller* than before — the exact inverse of the change.
  //
  // Measured 2026-09-01 at this viewport: dock 317, plot 204, chrome **113** — the estimate was
  // 7px conservative, which is the safe direction. The assertion is on the budget rather than on
  // 113, because a chrome that shrinks is not a regression; only one that eats the floor is.
  expect(
    chrome,
    'the dock chrome now costs more than the floor can pay: DOCK_MIN_PX would leave the charts ' +
      'shorter than they were before the dock shipped, which is what that constant exists to stop',
  ).toBeLessThanOrEqual(DOCK_MIN_PX - CHART_HEIGHT_BEFORE);
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

// ── What the charts say, as opposed to how big they are ──────────────────────────────────────────
// Everything above is layout. The three below are about the readout, and they are here for the same
// structural reason: uPlot draws to a canvas and keeps its legend in real DOM driven by a cursor,
// so neither the merged chart's key nor the legend's idle state nor cursor sync is reachable from
// `environment: 'node'`. `legend.test.ts` proves which index the idle legend should read; only this
// file can prove that index is connected to uPlot at all.

/** The uPlot legend's value cells for one dock chart, top row (time) first. */
function legendValues(page: import('@playwright/test').Page, chart: number) {
  return page.locator('.nd-if-chart').nth(chart).locator('.u-legend .u-value');
}

test('errors and discards share one chart, with a distinct key per line', async ({ page }) => {
  await openDock(page);
  await expect(page.locator('.nd-if-chart')).toHaveCount(2);

  const faults = page.locator('.nd-if-chart').nth(1).locator('.nd-if-chart-t');
  await expect(faults).toContainText('Errors / discards');
  await expect(faults.locator('.nd-if-legend-k')).toHaveCount(4);

  // Four lines that overlap at zero on every healthy interface: colour is the only thing telling
  // them apart once one rises, so two sharing a swatch is a legend that lies under load.
  // `FAULT_SERIES`' unit test pins distinct palette *slots*; this pins that they resolve to
  // distinct *colours* — the tokens are read from computed style, which no unit test can do.
  const colors = await faults
    .locator('.nd-if-legend-sw')
    .evaluateAll((els) => els.map((e) => getComputedStyle(e).backgroundColor));
  expect(new Set(colors).size, `swatch colours: ${colors.join(', ')}`).toBe(4);
});

// ADR-060 shipped with this unverified, on the belief that nothing here opens the dock. It does —
// `openDock` above has since this file was written — so the four points that were waiting on a
// person are three points and a language check.
test('the bps/pps toggle swaps the unit and takes the bandwidth overlay with it', async ({
  page,
}) => {
  await openDock(page);
  const head = page.locator('.nd-if-chart').first().locator('.nd-if-chart-t');

  await expect(head).toContainText('(In / Out, bps)');
  await expect(head.getByRole('button', { name: /auto|bandwidth/i })).toBeVisible();
  await expect(head.locator('.nd-if-legend-bw')).toHaveCount(1);

  await head.getByRole('button', { name: 'bps' }).click();

  await expect(head).toContainText('(In / Out, pps)');
  // `ifSpeed` is a bit rate, so on a packet axis the reference line would draw a capacity the
  // operator is nowhere near. `throughputBandwidthOverlay` returns `{}` — which its unit test
  // proves — but that `{}` reaching the JSX that removes the control and the key is `.tsx`, and
  // this is the only thing that runs it.
  await expect(head.getByRole('button', { name: /auto|bandwidth/i })).toHaveCount(0);
  await expect(head.locator('.nd-if-legend-bw')).toHaveCount(0);
});

test('both legends report the latest sample with no cursor on the page', async ({ page }) => {
  await openDock(page);
  for (const chart of [0, 1]) {
    const cells = legendValues(page, chart);
    await expect(cells.first()).not.toHaveText('--');
    const values = await cells.allTextContents();
    expect(values.length, `chart ${chart} has no legend rows`).toBeGreaterThan(1);
    // uPlot's live legend is blank without a cursor, which is the state a chart is in almost all
    // the time. A single `--` anywhere means the idle index never reached it.
    expect(values, `chart ${chart} legend is blank while unhovered`).not.toContain('--');
  }
});

test('hovering one chart reads the other at the same instant', async ({ page }) => {
  await openDock(page);
  const timeOf = (chart: number) => legendValues(page, chart).first();

  const idle = await timeOf(0).innerText();
  expect(await timeOf(1).innerText(), 'the two charts idle at different samples').toBe(idle);

  // Hover the throughput chart well left of its latest sample, so "the legend moved" is decidable.
  const box = (await page.locator('.nd-if-chart').nth(0).locator('.u-over').boundingBox())!;
  await page.mouse.move(box.x + box.width * 0.25, box.y + box.height / 2);

  await expect(timeOf(0), 'hover did not move the hovered chart').not.toHaveText(idle);
  // The point of the sync: the chart nobody touched follows to the same instant. Without it an
  // operator comparing a traffic spike against discards is reading two different moments.
  await expect(timeOf(1)).toHaveText(await timeOf(0).innerText());

  // And back: leaving must restore the latest sample rather than leaving the crosshair's values
  // frozen on screen, which would read as live data that has stopped updating.
  await page.mouse.move(box.x + box.width / 2, box.y - 200);
  await expect(timeOf(0)).toHaveText(idle);
  await expect(timeOf(1)).toHaveText(idle);
});

// Not asserted, and worth saying why: **that the list does not scroll during a drag**. The generated
// mock answers `/interfaces` with exactly one row, so `.nd-if-list` has nothing to scroll and the
// defect the drag-suppression flag exists to prevent cannot be reproduced here. Revisit if the
// fixture ever grows past one interface.
