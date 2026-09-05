// SPDX-License-Identifier: AGPL-3.0-only
// Optical transmit/receive power in the interface dock (ADR-062), in a real layout engine.
//
// The rule under test is the whole feature: **the optical chart exists for a port that reports
// light and does not exist for one that does not.** There is no "is optical" flag anywhere — the
// data having arrived is the evidence — so the only way to prove the rule is to serve two ports
// that differ in exactly that and look at what renders.
//
// Vitest already covers `hasOpticalData` and `opticalYRange` as pure functions. What it structurally
// cannot cover is whether those answers are *connected*: `InterfacesTab` is a `.tsx`, and Vitest
// runs `environment: 'node'` and never executes one. A predicate that is correct and unwired looks
// identical to a correct feature from the unit tests alone.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** ifIndex of the port wired with an optical transceiver in this fixture. */
const OPTICAL_IF = 1;
/** ifIndex of the copper port beside it — same shape, no optical arrays at all. */
const COPPER_IF = 2;

/** Only a `device` node shows the Interfaces tab at all. */
/** Two interfaces, so the presence *and* the absence of the chart are both observable in one run.
 *
 *  The generator emits a single row with placeholder values; a one-port fixture could only ever
 *  show that the chart appears, never that it is correctly withheld — and "renders for everything"
 *  is precisely the bug a presence-only test would pass. */
function interfaceRows(): Json {
  const [row] = defaultBodyFor(`/api/v1/nodes/${NODE_ID}/interfaces`) as Record<string, unknown>[];
  return [
    {
      ...row,
      ifindex: OPTICAL_IF,
      if_name: 'Te1/1/1',
      if_alias: 'uplink (fibre)',
      // The module's own window (ADR-062 Inc.4). Deliberately far below the readings, which is
      // the realistic case — a healthy link sits well inside its budget — and the case that
      // catches a chart whose Y range ignores the band: it would clamp to zero height and vanish.
      rx_power_low_dbm: -24,
      rx_power_high_dbm: -3,
      tx_power_low_dbm: -9,
      tx_power_high_dbm: -1,
    },
    {
      ...row,
      ifindex: COPPER_IF,
      if_name: 'Gi0/0/2',
      if_alias: 'LAN (copper)',
      rx_power_low_dbm: null,
      rx_power_high_dbm: null,
      tx_power_low_dbm: null,
      tx_power_high_dbm: null,
    },
  ] as unknown as Json;
}

/** A series covering the window the client actually asked for, optical only for [`OPTICAL_IF`].
 *
 *  ⚠️ Reading `from`/`to` off the request rather than inventing a window is load-bearing, for the
 *  reason `interfaceDock.spec.ts` documents: the generated mock puts its single sample at Unix
 *  second 1, an hour of 1970 outside the requested range, and uPlot then reports no data in range. */
function seriesBody(url: URL): Json {
  const from = Number(url.searchParams.get('from'));
  const to = Number(url.searchParams.get('to'));
  const n = 12;
  const timestamps = Array.from({ length: n }, (_, i) =>
    Math.round(from + ((to - from) * i) / (n - 1)),
  );
  const ramp = (base: number) => timestamps.map((_, i) => base * (i + 1));
  const optical = url.pathname.includes(`/interfaces/${OPTICAL_IF}/series`);
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
    // ⚠️ Negative, and drifting downward — what a receive level actually looks like, and what a
    // fixture built from the generator's positive `1`s would never exercise. A copper port gets
    // `null` throughout rather than a missing key: that is the shape a real core sends, since the
    // arrays are always present and simply carry no samples.
    rx_power_dbm: timestamps.map((_, i) => (optical ? -7.4 - i * 0.1 : null)),
    tx_power_dbm: timestamps.map(() => (optical ? -2.1 : null)),
  } as unknown as Json;
}

test.use({
  viewport: { width: 1600, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces': () => interfaceRows(),
      '/api/v1/nodes/{node_id}/interfaces/{ifindex}/series': (url: URL) => seriesBody(url),
    },
  },
});

/** Open the Interfaces tab and select the row whose name is `ifName`, which mounts the dock. */
async function openDock(page: import('@playwright/test').Page, ifName: string) {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await page.locator('.nd-if-row').filter({ hasText: ifName }).first().click();
  await expect(page.locator('.nd-if-dock')).toBeVisible({ timeout: 15_000 });
}

test('an optical port gets a third chart, and it is drawn', async ({ page }) => {
  await openDock(page, 'Te1/1/1');

  const charts = page.locator('.nd-if-dock-charts > .nd-if-chart');
  await expect(charts).toHaveCount(3);

  // Not just "a third box exists" — the box must be the optical one and must have a plot in it.
  // A chart that mounted and drew nothing is the failure mode `hasOpticalData` being wired to the
  // wrong array would produce, and a count assertion alone would pass straight through it.
  const optical = charts.filter({ hasText: /dBm/ });
  await expect(optical).toHaveCount(1);
  await expect(optical.locator('.metricchart-fill')).toBeVisible();
  await expect(optical.locator('canvas').first()).toBeVisible();
});

test('a copper port in the same device gets no optical chart at all', async ({ page }) => {
  await openDock(page, 'Gi0/0/2');

  // The dock must still be the two it has always been — not two-and-an-empty-frame. An optical
  // card rendering its "no data" placeholder for every copper port in the fleet would be the
  // quiet, universal regression here, and it would satisfy any assertion about the optical port.
  await expect(page.locator('.nd-if-dock-charts > .nd-if-chart')).toHaveCount(2);
  await expect(page.locator('.nd-if-dock-charts')).not.toContainText('dBm');
});

test('the optical level is reported as a signed dBm figure, not an SI-scaled one', async ({
  page,
}) => {
  await openDock(page, 'Te1/1/1');

  // The stat tiles are where the formatter is visible as text. `formatSi`/`formatBps` would render
  // these as `-7` or `-7 bps`; only `formatDbm` produces a signed figure with one decimal and the
  // unit — which is also the check that the receive and transmit tiles were not transposed, since
  // the fixture gives them deliberately different values.
  const stats = page.locator('.nd-if-dock-stats');
  await expect(stats).toContainText('-8.5 dBm');
  await expect(stats).toContainText('-2.1 dBm');
});

test("the module's own window is shown beside the reading", async ({ page }) => {
  await openDock(page, 'Te1/1/1');

  // The band itself is painted on canvas and cannot be asserted as an element — `.u-axis` carries
  // no text either, because uPlot draws its ticks into the canvas too. The window is therefore
  // *also* rendered as text, which is both the precise form for a reader and the only form a test
  // can see. This is what proves the interface row's limits reached the component at all: the
  // shading and this string are built from the same `opticalBandSpecs`.
  const stats = page.locator('.nd-if-dock-stats');
  await expect(stats).toContainText('-24.0 dBm … -3.0 dBm');
  await expect(stats).toContainText('-9.0 dBm … -1.0 dBm');
});

test('three charts wrap to a second row instead of overflowing a narrow dock', async ({ page }) => {
  await openDock(page, 'Te1/1/1');

  const charts = page.locator('.nd-if-dock-charts > .nd-if-chart');
  const wide = await charts.first().boundingBox();
  const third = await charts.nth(2).boundingBox();
  // Wide viewport: all three share one row, so the third starts at the same y as the first.
  expect(third!.y, 'expected one row at 1600px').toBeCloseTo(wide!.y, 0);

  await page.setViewportSize({ width: 900, height: 1000 });
  // ⚠️ The assertion is about the *layout*, so wait for the grid to settle rather than for a
  // network event — nothing refetches on a resize.
  await expect
    .poll(async () => (await charts.nth(2).boundingBox())!.y, { timeout: 5_000 })
    .toBeGreaterThan((await charts.first().boundingBox())!.y);

  // And the point of wrapping: the row must not have simply overflowed to the right instead.
  const grid = (await page.locator('.nd-if-dock-charts').boundingBox())!;
  const box = (await charts.nth(2).boundingBox())!;
  expect(box.x + box.width).toBeLessThanOrEqual(grid.x + grid.width + 1);
});
