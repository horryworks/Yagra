// SPDX-License-Identifier: AGPL-3.0-only
// The In/Out traffic columns and their utilization wash (ADR-126), in a real layout engine.
//
// The rule under test is the whole feature: a cell is shaded by how full its link is, and a port
// with no known rate is not shaded at all. Vitest already covers `utilHeat` and `trafficCell` as
// pure functions, and that is structurally not enough twice over. `InterfacesTab` is a `.tsx`, so
// Vitest never executes the wiring — a correct predicate that is not connected looks identical
// from the unit tests. And `utilHeat` returns a `color-mix(...)` STRING; whether that string
// resolves to a colour, and whether the two ends of the ramp resolve to different colours, is a
// question only a browser with the stylesheet loaded can answer.
//
// 🚨 The failure this exists for is not "no colour". It is a wash that resolves to the same value
// everywhere — a token that does not exist resolves to nothing, and `color-mix` with an invalid
// operand drops the whole declaration, which looks exactly like a healthy unshaded table. So every
// assertion below compares cells against each other rather than checking that a colour is present.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** Four ports chosen so every branch of the ramp is on screen at once, and so the two that must
 *  NOT be shaded are present beside the two that must. A fixture with only busy ports could not
 *  tell "shades correctly" from "shades everything". */
function interfaceRows(): Json {
  const [row] = defaultBodyFor(`/api/v1/nodes/${NODE_ID}/interfaces`) as Record<string, unknown>[];
  const port = (o: Record<string, unknown>) => ({ ...row, oper_status: 1, stale: false, ...o });
  return [
    // Nearly saturated: the hot end of the ramp.
    port({
      ifindex: 1,
      if_name: 'Gi0/0/1',
      if_alias: 'uplink',
      if_speed_bps: 1_000_000_000,
      in_bps: 9.6e8,
      out_bps: 9.4e8,
      in_util_pct: 96,
      out_util_pct: 94,
    }),
    // Quiet, but on a link whose rate is known: the cool end. Same absolute order of magnitude as
    // the unknown-speed port below, so an implementation reading bps instead of utilization would
    // shade those two the same and be caught.
    port({
      ifindex: 2,
      if_name: 'Gi0/0/2',
      if_alias: 'desk',
      if_speed_bps: 1_000_000_000,
      in_bps: 1.2e7,
      out_bps: 3.1e6,
      in_util_pct: 1.2,
      out_util_pct: 0.31,
    }),
    // No advertised rate — the case that must stay unpainted. A null utilization is what a real
    // core sends here; it declines to divide rather than reporting 0.
    port({
      ifindex: 3,
      if_name: 'Vlan100',
      if_alias: 'no rate',
      if_speed_bps: null,
      in_bps: 1.4e7,
      out_bps: 4.0e6,
      in_util_pct: null,
      out_util_pct: null,
    }),
    // Down. Shows the word `down` and must carry no wash, which is what keeps the cell colour from
    // ever contradicting the row's own StatusDot.
    port({
      ifindex: 4,
      if_name: 'Gi0/0/4',
      if_alias: 'unused',
      oper_status: 2,
      if_speed_bps: 1_000_000_000,
      in_bps: null,
      out_bps: null,
      in_util_pct: null,
      out_util_pct: null,
    }),
  ] as unknown as Json;
}

test.use({
  viewport: { width: 1600, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces': () => interfaceRows(),
    },
  },
});

type Page = import('@playwright/test').Page;

async function openTab(page: Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await expect(page.locator('.nd-if-row')).toHaveCount(4, { timeout: 15_000 });
}

/** The computed background of one direction's cell on the row named `ifName`. */
function cellBg(page: Page, ifName: string, dir: 'in' | 'out') {
  return page
    .locator('.nd-if-row')
    .filter({ hasText: ifName })
    .first()
    .locator(`.nd-if-${dir}`)
    .evaluate((el) => getComputedStyle(el).backgroundColor);
}

/** A CSS colour string resolved to 8-bit RGBA, by letting the browser paint it.
 *
 * ⚠️ Do NOT parse the string. Chromium returns the result of a `color-mix(in oklab, …)` as
 * `oklab(L a b / alpha)`, not as `rgb()` — so a regex that reads the first three numbers as red,
 * green and blue reads lightness and two opponent axes instead, and compares nonsense while
 * looking like it works. Painting onto a 1x1 canvas asks the same engine that drew the cell.
 *
 * A colour the canvas cannot parse leaves `fillStyle` at its default opaque black, which fails the
 * comparisons below rather than passing them — the safe direction for a helper like this. */
async function channels(
  page: Page,
  css: string,
): Promise<{ r: number; g: number; b: number; a: number }> {
  return page.evaluate((c) => {
    const cv = document.createElement('canvas');
    cv.width = 1;
    cv.height = 1;
    const ctx = cv.getContext('2d')!;
    ctx.clearRect(0, 0, 1, 1);
    ctx.fillStyle = c;
    ctx.fillRect(0, 0, 1, 1);
    const d = ctx.getImageData(0, 0, 1, 1).data;
    return { r: d[0], g: d[1], b: d[2], a: d[3] / 255 };
  }, css);
}

/** The painted RGBA of one direction's cell on the row named `ifName`. */
async function cellRgba(page: Page, ifName: string, dir: 'in' | 'out') {
  return channels(page, await cellBg(page, ifName, dir));
}

test('In and Out are two columns, not one cell holding both', async ({ page }) => {
  await openTab(page);

  // The header names them separately...
  const heads = page.locator('.nd-if-head .nd-if-h');
  await expect(heads).toHaveCount(9);
  await expect(heads.nth(7)).toHaveText('In');
  await expect(heads.nth(8)).toHaveText('Out');

  // ...and each row carries one cell per direction, holding its own figure. The old shape put both
  // in one cell separated by a slash, which is what a text assertion on the row alone would miss.
  const busy = page.locator('.nd-if-row').filter({ hasText: 'Gi0/0/1' }).first();
  await expect(busy.locator('.nd-if-in')).toHaveCount(1);
  await expect(busy.locator('.nd-if-out')).toHaveCount(1);
  expect(await busy.locator('.nd-if-in').innerText()).not.toContain('/');
});

test('the filter controls still sit under their own headers with nine columns', async ({
  page,
}) => {
  await openTab(page);
  // The header, the filter row and every data row share ONE grid template. Splitting a column is
  // exactly the edit that desynchronises them, and the symptom is silent: controls slide out from
  // under their headings. Compare the resolved track lists rather than the declaration.
  const [head, filters, row] = await Promise.all(
    ['.nd-if-head', '.nd-if-filters', '.nd-if-row'].map((sel) =>
      page
        .locator(sel)
        .first()
        .evaluate((el) => getComputedStyle(el).gridTemplateColumns),
    ),
  );
  expect(head.split(' ')).toHaveLength(9);
  expect(filters).toBe(head);
  expect(row).toBe(head);
});

test('a saturated port reads red and a quiet one reads green, on the same link rate', async ({
  page,
}) => {
  await openTab(page);

  const hot = await cellRgba(page, 'Gi0/0/1', 'in');
  const cool = await cellRgba(page, 'Gi0/0/2', 'in');

  // Both are painted at all — a dropped declaration would leave both fully transparent.
  expect(hot.a, 'the 96% cell is unpainted').toBeGreaterThan(0);
  expect(cool.a, 'the 1.2% cell is unpainted').toBeGreaterThan(0);

  // 🚨 Not "both are coloured" — they must be coloured DIFFERENTLY, and in the right directions.
  // A ramp collapsed to one colour satisfies every presence assertion.
  expect(hot.r, 'the busy cell is not redder than the quiet one').toBeGreaterThan(cool.r);
  expect(cool.g, 'the quiet cell is not greener than the busy one').toBeGreaterThan(hot.g);
  // And each is on the side of the ramp it claims: red dominant at the top, green at the bottom.
  expect(hot.r).toBeGreaterThan(hot.g);
  expect(cool.g).toBeGreaterThan(cool.r);
});

test('a port that never advertised a rate is left unshaded', async ({ page }) => {
  await openTab(page);

  // 14 Mbit/s — more absolute traffic than the quiet gigabit port above, so an implementation that
  // ramped on bps instead of utilization would shade this one and fail here.
  const noRate = await cellRgba(page, 'Vlan100', 'in');
  expect(noRate.a, 'a port with no known rate was shaded anyway').toBe(0);

  const quiet = await cellRgba(page, 'Gi0/0/2', 'in');
  expect(quiet.a, 'the comparison is vacuous — nothing on this table is shaded').toBeGreaterThan(0);
});

test('a down port shows the word and carries no wash', async ({ page }) => {
  await openTab(page);

  const down = page.locator('.nd-if-row').filter({ hasText: 'Gi0/0/4' }).first();
  await expect(down.locator('.nd-if-in')).toHaveText('down');
  await expect(down.locator('.nd-if-out')).toHaveText('down');
  // If this were shaded, a cell could read green beside a StatusDot reading down — two colour
  // vocabularies disagreeing on one row, which is what the token comment forbids.
  expect((await cellRgba(page, 'Gi0/0/4', 'in')).a).toBe(0);
});

test('the two directions of one port shade independently', async ({ page }) => {
  await openTab(page);

  // 96% in against 94% out: close, deliberately. A cell wired to read `in_util_pct` for both
  // directions would still look plausible on a table of round numbers; it cannot be plausible and
  // also produce two different colours from two different readings.
  const inBg = await cellBg(page, 'Gi0/0/1', 'in');
  const outBg = await cellBg(page, 'Gi0/0/1', 'out');
  expect(inBg).not.toBe(outBg);
});

test('the wash is translucent, so the row hover shows through it', async ({ page }) => {
  await openTab(page);

  // 🚨 The wash mixes toward `transparent` precisely so the row's hover colour survives underneath.
  // Mixing toward `--bg-secondary` instead would look identical on a still screenshot and silently
  // kill hover feedback on two of the nine cells — the defect `.dt-row` shipped for months.
  //
  // ⚠️ This cannot be asserted by hovering and re-reading the CELL: `getComputedStyle` returns an
  // element's own background, and the cell's does not change when its row is hovered. The two
  // halves of the invariant have to be read separately — the row's colour changes, and the cell
  // above it is not opaque.
  const row = page.locator('.nd-if-row').filter({ hasText: 'Gi0/0/1' }).first();
  const rowBg = () => row.evaluate((el) => getComputedStyle(el).backgroundColor);

  const restingRow = await rowBg();
  await row.hover();
  await expect.poll(rowBg, { timeout: 5_000 }).not.toBe(restingRow);

  // And the shaded cell lets it through. Alpha strictly below 1 is the whole property; the ramp's
  // top end is 30%, so anything at or near 1 means the mix lost its transparent operand.
  const hot = await cellRgba(page, 'Gi0/0/1', 'in');
  expect(hot.a, 'the wash is opaque and would hide the row hover').toBeLessThan(0.5);
  expect(hot.a, 'the wash is invisible').toBeGreaterThan(0);
});
