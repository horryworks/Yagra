// SPDX-License-Identifier: AGPL-3.0-only
// The Interfaces list's MODE and VLAN columns, the member link and the dock tiles (ADR-201), in a
// real layout engine.
//
// Vitest covers what the cells say (`interfaceVlan.ts`). What it cannot reach is the wiring in
// `InterfacesTab.tsx`, and the parts that fail silently are the ones ADR-145 and ADR-157 met: a
// control inside a row whose own click opens the dock, the filter reading a value the row alone
// cannot see (a member's VLANs are its aggregate's), and two new tracks in a template three grids
// share.
//
// The rows are the shapes walked for ADR-201 with made-up names and doc-range addresses: an
// Eth-Trunk and two members, an access port with a voice VLAN, a trunk allowing everything, a routed
// port, and a port whose VLANs were never reported.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ad';

const NO_VLAN = {
  native: null,
  access_vlan: null,
  voice_vlan: null,
  allowed: [],
  untagged: [],
  tagged: [],
  lag: null,
  members: [],
};

function interfaceRows(): Json {
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
    addresses: [],
    ...o,
  });
  return [
    port({
      ifindex: 215,
      if_name: 'Eth-Trunk0',
      vlan: {
        ...NO_VLAN,
        mode: 'trunk',
        native: 1,
        allowed: [
          { first: 700, last: 700 },
          { first: 801, last: 869 },
        ],
        members: [
          { ifindex: 55, name: 'XGE0/0/1' },
          { ifindex: 159, name: 'XGE2/0/1' },
        ],
      },
    }),
    port({
      ifindex: 55,
      if_name: 'XGE0/0/1',
      vlan: { ...NO_VLAN, mode: 'member', lag: { ifindex: 215, name: 'Eth-Trunk0' } },
    }),
    port({
      ifindex: 159,
      if_name: 'XGE2/0/1',
      vlan: { ...NO_VLAN, mode: 'member', lag: { ifindex: 215, name: 'Eth-Trunk0' } },
    }),
    port({
      ifindex: 7,
      if_name: 'GE0/0/7',
      vlan: { ...NO_VLAN, mode: 'access', access_vlan: 100, voice_vlan: 200 },
    }),
    port({
      ifindex: 24,
      if_name: 'GE0/0/24',
      vlan: { ...NO_VLAN, mode: 'trunk', native: null, allowed: [{ first: 1, last: 4094 }] },
    }),
    port({
      ifindex: 40,
      if_name: 'Vlanif100',
      addresses: [{ ip: '192.0.2.1', prefix_len: 24 }],
      vlan: { ...NO_VLAN, mode: 'not_l2' },
    }),
    port({ ifindex: 3, if_name: 'GE0/0/3', vlan: null }),
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

const row = (page: Page, name: string) =>
  page.locator('.nd-if-row').filter({ has: page.locator('.nd-if-name', { hasText: name }) });

test('each port says its mode and its VLANs, and Oper sits beside the name', async ({ page }) => {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.locator('.nd-if-row')).toHaveCount(7, { timeout: 15_000 });

  const heads = page.locator('.nd-if-head .nd-if-h');
  await expect(heads.nth(1)).toHaveText('Oper');
  await expect(heads.nth(8)).toHaveText('Mode');
  await expect(heads.nth(9)).toHaveText('VLAN');

  await expect(row(page, 'Eth-Trunk0').locator('.nd-if-mode')).toHaveText('trunk');
  await expect(row(page, 'Eth-Trunk0').locator('.nd-if-vlan')).toHaveText(
    'native 1 · 700,801-869',
  );
  await expect(row(page, 'XGE0/0/1').locator('.nd-if-vlan')).toHaveText('in Eth-Trunk0');
  await expect(row(page, 'GE0/0/7').locator('.nd-if-vlan')).toHaveText('100 + voice 200');
  await expect(row(page, 'GE0/0/24').locator('.nd-if-vlan')).toHaveText('no native · all');
  await expect(row(page, 'Vlanif100').locator('.nd-if-vlan')).toHaveText('n/a');
  await expect(row(page, 'GE0/0/3').locator('.nd-if-mode')).toHaveText('not reported');
});

test('a member’s aggregate link selects the aggregate, and its dock lists the members', async ({
  page,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces`);
  await expect(page.locator('.nd-if-row')).toHaveCount(7, { timeout: 15_000 });

  await row(page, 'XGE2/0/1').locator('.nd-if-vlan-lag').click();
  // The aggregate's row, not the member's: the link stops its click, so the row under it does
  // not open its own dock as well.
  await expect(page.locator('.nd-if-row.selected .nd-if-name')).toHaveText('Eth-Trunk0');
  const dock = page.locator('.nd-if-dock-head');
  await expect(dock).toContainText('native 1 · 700,801-869');
  await expect(dock).toContainText('70 VLANs');

  await dock.locator('.nd-if-vlan-lag', { hasText: 'XGE0/0/1' }).click();
  await expect(page.locator('.nd-if-row.selected .nd-if-name')).toHaveText('XGE0/0/1');
});

test('a VLAN ID narrows to the ports carrying it, members through their aggregate', async ({
  page,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces&interfaces.vlan=850`);
  const names = page.locator('.nd-if-row .nd-if-name');
  await expect(names).toHaveText(['Eth-Trunk0', 'XGE0/0/1', 'XGE2/0/1', 'GE0/0/24'], {
    timeout: 15_000,
  });

  await page.goto(`/nodes/${NODE_ID}?tab=interfaces&interfaces.vlan=200`);
  await expect(names).toHaveText(['GE0/0/7', 'GE0/0/24'], { timeout: 15_000 });
});

test('header, filter row and data rows still share one template with the two new tracks', async ({
  page,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=interfaces&interfaces.mode=trunk`);
  await expect(page.locator('.nd-if-row')).toHaveCount(2, { timeout: 15_000 });
  const [head, filters, dataRow] = await Promise.all(
    ['.nd-if-head', '.nd-if-filters', '.nd-if-row'].map((sel) =>
      page
        .locator(sel)
        .first()
        .evaluate((el) => getComputedStyle(el).gridTemplateColumns),
    ),
  );
  expect(head.split(' ')).toHaveLength(13);
  expect(filters).toBe(head);
  expect(dataRow).toBe(head);
});
