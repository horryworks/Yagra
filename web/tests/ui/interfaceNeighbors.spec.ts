// SPDX-License-Identifier: AGPL-3.0-only
// The Interfaces list's Neighbors column and its popover (ADR-145), in a real layout engine.
//
// Vitest covers which neighbour belongs to which port (`neighbors.ts`). What it cannot reach is the
// wiring in `InterfacesTab.tsx`, and two parts of that wiring fail silently:
//
//  - The row used to be a <button>, and it became a <div> so the cell could hold a control. The
//    row must still open the dock — by a click and by the keyboard — and the link must NOT. A React
//    event from a portalled popover bubbles through the React tree, not the DOM tree, so a click
//    inside the popover reaches the row's handler unless something stops it. Nothing on screen
//    would say so except a dock opening under the operator's pointer.
//  - Escape is claimed by both the popover and the dock. It must close the popover and leave the
//    dock alone, which `escapeDismiss.ts` layers but nothing else checks for this pair.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ab';

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
    ...o,
  });
  return [
    port({ ifindex: 1, if_name: 'Gi0/0/1', if_alias: 'uplink' }),
    port({ ifindex: 2, if_name: 'Gi0/0/2', if_alias: 'access points' }),
    port({ ifindex: 3, if_name: 'Gi0/0/3', if_alias: 'spare' }),
  ] as unknown as Json;
}

/** One neighbour placed each way the column can place one, and one it must place nowhere. */
function currentNeighbors(): Json {
  const base = {
    remote_port_desc: null,
    remote_sys_desc: null,
    remote_mgmt_addr: null,
    remote_platform: null,
    capabilities: [],
  };
  return {
    first_seen: '2026-09-15T00:00:00Z',
    last_seen: '2026-09-15T00:00:00Z',
    neighbors: {
      truncated: false,
      neighbors: [
        // CDP, by ifIndex — its port name is deliberately NOT the row's, so a name match cannot
        // be what placed it.
        {
          ...base,
          proto: 'cdp',
          local_port: 'GigabitEthernet0/0/1',
          local_ifindex: 1,
          remote_chassis: 'core-sw01.example',
          remote_sys_name: 'core-sw01',
          remote_port: 'Gi1/0/24',
          remote_mgmt_addr: '10.0.0.1',
          capabilities: ['router', 'switch'],
        },
        // Two LLDP neighbours on one port, by name (one in another case).
        {
          ...base,
          proto: 'lldp',
          local_port: 'gi0/0/2',
          local_ifindex: null,
          remote_chassis: 'aa:bb:cc:00:00:01',
          remote_sys_name: 'ap-3f-01',
          remote_port: 'eth0',
          capabilities: ['wlan_ap'],
        },
        {
          ...base,
          proto: 'lldp',
          local_port: 'Gi0/0/2',
          local_ifindex: null,
          remote_chassis: 'aa:bb:cc:00:00:02',
          remote_sys_name: 'ap-3f-02',
          remote_port: 'eth0',
          capabilities: ['wlan_ap'],
        },
        // LLDP naming the port by number. Row 3 has ifIndex 3, and it must still show a dash.
        {
          ...base,
          proto: 'lldp',
          local_port: '3',
          local_ifindex: null,
          remote_chassis: 'aa:bb:cc:00:00:03',
          remote_sys_name: 'phone-9',
          remote_port: 'p1',
        },
      ],
    },
  } as unknown as Json;
}

test.use({
  viewport: { width: 1600, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/interfaces': () => interfaceRows(),
      '/api/v1/nodes/{node_id}/neighbors': () => currentNeighbors(),
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

test('each port names the device it faces, and a port with none shows a dash', async ({ page }) => {
  await openTab(page);
  await expect(page.locator('.nd-if-head .nd-if-h').nth(2)).toHaveText('Neighbors');

  // Waiting on the populated cell first: the neighbours arrive after the rows, and a dash read
  // before they land would pass the third assertion for the wrong reason.
  await expect(row(page, 'Gi0/0/1').locator('.nd-if-nb-link')).toHaveText('core-sw01');
  await expect(row(page, 'Gi0/0/2').locator('.nd-if-nb-link')).toHaveText('ap-3f-01');
  await expect(row(page, 'Gi0/0/2').locator('.nd-if-nb-more')).toHaveText('+1');
  await expect(row(page, 'Gi0/0/3').locator('.nd-if-nb-link')).toHaveCount(0);
  await expect(row(page, 'Gi0/0/3').locator('.nd-if-nb')).toHaveText('—');

  // The title carries every neighbour, because the cell ellipsizes.
  await expect(row(page, 'Gi0/0/2').locator('.nd-if-nb-link')).toHaveAttribute(
    'title',
    'ap-3f-01 eth0\nap-3f-02 eth0',
  );
});

test('the name opens the port’s neighbours beside it, and never the dock', async ({ page }) => {
  await openTab(page);
  await row(page, 'Gi0/0/2').locator('.nd-if-nb-link').click();

  const pop = page.getByRole('dialog', { name: 'Neighbors on Gi0/0/2' });
  await expect(pop).toBeVisible();
  await expect(pop).toContainText('ap-3f-01');
  await expect(pop).toContainText('ap-3f-02');
  await expect(pop).toContainText('Wi-Fi AP');
  await expect(page.locator('.nd-if-dock')).toHaveCount(0);

  // 🚨 A click inside the portalled panel bubbles to the row through the React tree.
  await pop.getByText('ap-3f-02').click();
  await expect(pop).toBeVisible();
  await expect(page.locator('.nd-if-dock')).toHaveCount(0);

  await page.keyboard.press('Escape');
  await expect(pop).toHaveCount(0);
  await expect(page.locator('.nd-if-dock')).toHaveCount(0);
});

test('Escape closes the popover before the dock', async ({ page }) => {
  await openTab(page);
  await row(page, 'Gi0/0/1').locator('.nd-if-desc').click();
  await expect(page.locator('.nd-if-dock')).toBeVisible();

  await row(page, 'Gi0/0/1').locator('.nd-if-nb-link').click();
  const pop = page.getByRole('dialog', { name: 'Neighbors on Gi0/0/1' });
  await expect(pop).toContainText('10.0.0.1');

  await page.keyboard.press('Escape');
  await expect(pop).toHaveCount(0);
  await expect(page.locator('.nd-if-dock'), 'one Escape closed the dock too').toBeVisible();
});

test('the row still opens the dock, by a click and from the keyboard', async ({ page }) => {
  await openTab(page);

  await row(page, 'Gi0/0/1').locator('.nd-if-media').click();
  await expect(page.locator('.nd-if-dock-name')).toHaveText('Gi0/0/1');

  // The port name is the row's keyboard handle now that the row is not a button.
  await row(page, 'Gi0/0/2').locator('.nd-if-name').focus();
  await page.keyboard.press('Enter');
  await expect(page.locator('.nd-if-dock-name')).toHaveText('Gi0/0/2');
});
