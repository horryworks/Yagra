// SPDX-License-Identifier: AGPL-3.0-only
// The Interfaces list's IP addresses column, its popover and the dock tile (ADR-157), in a real
// layout engine.
//
// Vitest covers what the cell says (`interfaceAddresses.ts`). What it cannot reach is the wiring in
// `InterfacesTab.tsx`, and the parts of it that fail silently are the same ones ADR-145 met:
//
//  - The `+N` button sits inside a row whose click opens the dock. It must open the popover and
//    NOT the dock, and a click inside the portalled panel must not reach the row either.
//  - Escape is claimed by both the popover and the dock; it closes the popover and leaves the dock.
//  - The column is an eleventh grid track. The header, the filter row and every data row share one
//    template, and a column inserted third is exactly the edit that slides them apart.
//
// The fixture is the PoC recordings' shape in miniature (2026-09-17): an SVI carrying secondaries
// and a v6 address whose prefix the device did not give, a point-to-point /30, and a port with none.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ac';

interface Address {
  ip: string;
  prefix_len: number | null;
}

/** Three addresses: enough for `+N`, the popover and the unknown-prefix spelling. */
const SVI_SHORT: Address[] = [
  { ip: '10.104.29.254', prefix_len: 24 },
  { ip: '10.121.1.254', prefix_len: 24 },
  { ip: 'fec0::a:0:0:4', prefix_len: null },
];

/** The eleven a real SVI carries — `Vlanif100` on the PoC recording `sim-hw-ys1202`, verbatim.
 *  The phone test needs this one: three addresses fit a 390px line without wrapping, so a tile
 *  that could not wrap passed with the short list (found by breaking the CSS, 2026-09-17). */
const SVI_RECORDED: Address[] = [
  { ip: '10.104.29.254', prefix_len: 24 },
  ...[1, 2, 3, 4, 5, 6, 7, 8].map((n) => ({ ip: `10.121.${n}.254`, prefix_len: 24 })),
  { ip: '10.125.171.254', prefix_len: 24 },
  { ip: '10.125.187.254', prefix_len: 22 },
];

function interfaceRows(svi: Address[] = SVI_SHORT): Json {
  const [row] = defaultBodyFor(`/api/v1/nodes/${NODE_ID}/interfaces`) as Record<string, unknown>[];
  const port = (o: Record<string, unknown>) => ({
    ...row,
    oper_status: 1,
    stale: false,
    if_speed_bps: 1_000_000_000,
    in_bps: 1e6,
    out_bps: 1e6,
    in_util_pct: 0.1,
    out_util_pct: 0.1,
    ...o,
  });
  return [
    port({
      ifindex: 71,
      if_name: 'Vlanif100',
      if_alias: 'core svi',
      addresses: svi,
    }),
    port({
      ifindex: 81,
      if_name: 'Vlanif91',
      if_alias: 'to firewall',
      addresses: [{ ip: '10.103.250.42', prefix_len: 30 }],
    }),
    port({ ifindex: 3, if_name: 'Gi0/0/3', if_alias: 'spare', addresses: [] }),
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
  await expect(page.locator('.nd-if-row')).toHaveCount(3, { timeout: 15_000 });
}

const row = (page: Page, name: string) =>
  page.locator('.nd-if-row').filter({ hasText: name }).first();

test('each port shows its first address as ip/prefix, +N for the rest, and a dash for none', async ({
  page,
}) => {
  await openTab(page);
  await expect(page.locator('.nd-if-head .nd-if-h').nth(2)).toHaveText('IP addresses');

  const svi = row(page, 'Vlanif100');
  await expect(svi.locator('.nd-if-addr-first')).toHaveText('10.104.29.254/24');
  await expect(svi.locator('.nd-if-addr-more')).toHaveText('+2');
  // Hovering reads every address without opening anything; the unknown prefix is bare, not /0.
  await expect(svi.locator('.nd-if-addr')).toHaveAttribute(
    'title',
    '10.104.29.254/24\n10.121.1.254/24\nfec0::a:0:0:4',
  );

  // One address: shown whole, and nothing to open.
  const p2p = row(page, 'Vlanif91');
  await expect(p2p.locator('.nd-if-addr-first')).toHaveText('10.103.250.42/30');
  await expect(p2p.locator('.nd-if-addr-more')).toHaveCount(0);

  await expect(row(page, 'Gi0/0/3').locator('.nd-if-addr')).toHaveText('—');
});

test('+N opens every address beside the cell, and never the dock', async ({ page }) => {
  await openTab(page);
  await row(page, 'Vlanif100').locator('.nd-if-addr-more').click();

  const pop = page.getByRole('dialog', { name: 'IP addresses on Vlanif100' });
  await expect(pop).toBeVisible();
  await expect(pop.locator('.nd-if-addrpop-item')).toHaveText([
    '10.104.29.254/24',
    '10.121.1.254/24',
    'fec0::a:0:0:4',
  ]);
  await expect(page.locator('.nd-if-dock')).toHaveCount(0);

  // 🚨 A click inside the portalled panel bubbles to the row through the React tree.
  await pop.getByText('10.121.1.254/24').click();
  await expect(pop).toBeVisible();
  await expect(page.locator('.nd-if-dock')).toHaveCount(0);

  await page.keyboard.press('Escape');
  await expect(pop).toHaveCount(0);
  await expect(page.locator('.nd-if-dock')).toHaveCount(0);
});

test('Escape closes the address popover before the dock, and the dock lists every address', async ({
  page,
}) => {
  await openTab(page);
  // The address text is row, not control: clicking it opens the dock like any other cell.
  await row(page, 'Vlanif100').locator('.nd-if-addr-first').click();
  await expect(page.locator('.nd-if-dock-name')).toHaveText('Vlanif100');
  // The dock is the one place a single port is on screen, and the phone's only route to these.
  await expect(page.locator('.nd-if-dock-addrs')).toContainText(
    '10.104.29.254/24, 10.121.1.254/24, fec0::a:0:0:4',
  );

  await row(page, 'Vlanif100').locator('.nd-if-addr-more').click();
  const pop = page.getByRole('dialog', { name: 'IP addresses on Vlanif100' });
  await expect(pop).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(pop).toHaveCount(0);
  await expect(page.locator('.nd-if-dock'), 'one Escape closed the dock too').toBeVisible();
});

test('the address filter finds a secondary hidden behind +N', async ({ page }) => {
  await openTab(page);
  // `10.121.` is only in Vlanif100's SECOND address — the one the cell does not show.
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces&interfaces.addresses=10.121.`);
  await expect(page.locator('.nd-if-row')).toHaveCount(1);
  await expect(row(page, 'Vlanif100')).toBeVisible();
});

test('header, filter row and data rows still share one eleven-track template', async ({ page }) => {
  await openTab(page);
  const [head, filters, dataRow] = await Promise.all(
    ['.nd-if-head', '.nd-if-filters', '.nd-if-row'].map((sel) =>
      page
        .locator(sel)
        .first()
        .evaluate((el) => getComputedStyle(el).gridTemplateColumns),
    ),
  );
  expect(head.split(' ')).toHaveLength(11);
  expect(filters).toBe(head);
  expect(dataRow).toBe(head);
});

test('on the desktop the dock tile stays one line, so the dock chrome does not grow', async ({
  page,
}) => {
  await openTab(page);
  await row(page, 'Vlanif100').locator('.nd-if-addr-first').click();
  const tile = page.locator('.nd-if-dock-addrs');
  await expect(tile).toBeVisible();

  // 🚨 The first version of this tile wrapped (`overflow-wrap: anywhere` inside a nowrap flex row
  // shrinks a tile to one character wide), and the dock head went from one line to five — eating
  // the charts' floor, which `interfaceDock.spec.ts` measures. Compare against a sibling tile
  // rather than a pixel figure: both are one line of the same font, whatever that comes to.
  const [tileH, siblingH] = await Promise.all([
    tile.evaluate((el) => el.getBoundingClientRect().height),
    page
      .locator('.nd-if-dock-stats > span')
      .first()
      .evaluate((el) => el.getBoundingClientRect().height),
  ]);
  expect(tileH, 'the IP tile wrapped to more than one line').toBeLessThanOrEqual(siblingH + 2);
  // One line may ellipsize, so the whole list has to be in the title.
  await expect(tile).toHaveAttribute(
    'title',
    '10.104.29.254/24\n10.121.1.254/24\nfec0::a:0:0:4',
  );
});

test.describe('on a phone', () => {
  // Below `MOBILE_BP` (768). ⚠️ The viewport alone is not enough — the shared fixture seeds
  // `uiMode: 'desktop'`, which wins over the width (`overflowMenu.spec.ts` carries the same note),
  // so without this init script the test would measure a squeezed desktop layout and pass.
  test.use({ viewport: { width: 390, height: 844 } });

  // The recorded eleven-address SVI, not the short fixture: three addresses fit one 390px line,
  // so with them this test passed even with the tile unable to wrap.
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
        '/api/v1/nodes/{node_id}/interfaces': () => interfaceRows(SVI_RECORDED),
      },
    },
  });

  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => {
      localStorage.setItem(
        'yagra_prefs',
        JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode: 'auto' }, version: 0 }),
      );
    });
  });

  test('the column is not drawn and the dock shows every address, unclipped', async ({ page }) => {
    await openTab(page);
    // Pin the layer that was measured: a fixture change must not quietly turn this into a desktop test.
    expect(
      await page.evaluate(() => document.documentElement.getAttribute('data-viewport')),
    ).toBe('mobile');

    // `display: none`, read from the computed style — `isVisible()` would say the same here, but
    // the row is a named 2×2 grid and an address cell that was merely *unplaced* would auto-place
    // into a third row instead of disappearing.
    const cellDisplay = await row(page, 'Vlanif100')
      .locator('.nd-if-addr')
      .evaluate((el) => getComputedStyle(el).display);
    expect(cellDisplay).toBe('none');

    await row(page, 'Vlanif100').locator('.nd-if-name').click();
    const tile = page.locator('.nd-if-dock-addrs');
    // First and last of the eleven: the list is whole, not the list's "first +N".
    await expect(tile).toContainText('10.104.29.254/24, 10.121.1.254/24');
    await expect(tile).toContainText('10.125.187.254/22');

    // Every address is on screen: eleven cannot fit one 390px line, so the tile has to have
    // wrapped — more than one line tall, not ellipsized, and not pushed past the right edge.
    const geo = await tile.evaluate((el) => {
      const r = el.getBoundingClientRect();
      return {
        clipped: el.scrollWidth > el.clientWidth + 1,
        right: r.right,
        height: r.height,
        vw: document.documentElement.clientWidth,
      };
    });
    // One line is measured off a sibling tile, never off `line-height` — its computed value here
    // is the keyword `normal`, which parses to NaN (the first version of this test divided by it).
    const oneLine = await page
      .locator('.nd-if-dock-stats > span:not(.nd-if-dock-addrs)')
      .first()
      .evaluate((el) => el.getBoundingClientRect().height);
    expect(geo.clipped, 'the IP tile is cut off on a phone — its only route there').toBe(false);
    expect(geo.right, 'the IP tile runs off the right edge').toBeLessThanOrEqual(geo.vw + 1);
    expect(geo.height, 'eleven addresses on one line: the tile did not wrap').toBeGreaterThan(
      oneLine * 1.5,
    );
  });
});
