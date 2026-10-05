// SPDX-License-Identifier: AGPL-3.0-only
// The Neighbors tab when several nodes claim a neighbour's address (ADR-180 Inc.4), in a real
// layout engine.
//
// Vitest covers the judgement (`alsoClaimed`, `pickedByName` in `neighbors.ts`). What it cannot
// reach is that the tab draws it: the duplicate mark beside the name — kept even when the name
// picked the peer, which is the whole point of the increment — and the list in the opened row,
// which is where the mark's hover text has to be readable without a mouse (ADR-055 R4).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import type { Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ac';

const base = {
  remote_port_desc: null,
  remote_sys_desc: null,
  remote_platform: null,
  capabilities: [],
  local_ifindex: null,
};

const peerBase = {
  node_id: null,
  node_name: null,
  discovery_listed: false,
  discovery_id: null,
  managed_by: null,
  setup_blocked: null,
  matched_by_name: false,
  also_claimed_by: [],
  also_claimed_total: 0,
};

/** One row the name resolved among several claimants, and one it could not. */
function currentNeighbors(): Json {
  return {
    first_seen: '2026-09-30T00:00:00Z',
    last_seen: '2026-09-30T00:00:00Z',
    neighbors: {
      truncated: false,
      neighbors: [
        {
          ...base,
          proto: 'cdp',
          local_port: 'Gi0/24',
          remote_chassis: 'core-sw-01(FOC0000X0AB)',
          remote_chassis_kind: 'text',
          remote_sys_name: null,
          remote_port: 'Gi1/0/16',
          remote_mgmt_addr: '192.0.2.182',
        },
        {
          ...base,
          proto: 'lldp',
          local_port: 'Gi0/23',
          remote_chassis: 'aa:bb:cc:00:00:09',
          remote_chassis_kind: 'mac',
          remote_sys_name: 'fw-vip',
          remote_port: 'eth1',
          remote_mgmt_addr: '192.0.2.50',
        },
      ],
    },
    peers: [
      {
        ...peerBase,
        address: '192.0.2.182',
        state: 'node',
        node_id: '00000000-0000-4000-8000-000000000101',
        node_name: 'core-sw-01',
        matched_by_name: true,
        also_claimed_by: [
          {
            node_id: '00000000-0000-4000-8000-000000000102',
            node_name: 'wan-rtr-01',
            port_state: 'up',
          },
          {
            node_id: '00000000-0000-4000-8000-000000000103',
            node_name: 'wan-rtr-02',
            port_state: 'link_down',
          },
        ],
        // One more, in a folder this caller cannot see.
        also_claimed_total: 3,
      },
      {
        ...peerBase,
        address: '192.0.2.50',
        state: 'ambiguous',
        also_claimed_by: [
          {
            node_id: '00000000-0000-4000-8000-000000000104',
            node_name: 'fw-a',
            port_state: 'up',
          },
          {
            node_id: '00000000-0000-4000-8000-000000000105',
            node_name: 'fw-b',
            port_state: 'unknown',
          },
        ],
        also_claimed_total: 2,
      },
    ],
    chassis_peers: [],
    mac_vendors: [],
  } as unknown as Json;
}

test.use({
  viewport: { width: 1600, height: 1000 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/neighbors': () => currentNeighbors(),
    },
  },
});

type Page = import('@playwright/test').Page;

const row = (page: Page, port: string) =>
  page.locator('.dt-row').filter({ hasText: port }).first();

async function openTab(page: Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=neighbors`);
  await expect(page.locator('.dt-row').filter({ hasText: 'Gi0/24' })).toHaveCount(1, {
    timeout: 15_000,
  });
}

test('a peer picked by name keeps the duplicate mark beside it', async ({ page }) => {
  await openTab(page);
  const picked = row(page, 'Gi0/24');
  await expect(picked.locator('.nd-nb-state')).toHaveText('Monitored');
  await expect(picked.locator('.nd-nb-link')).toHaveCount(1);
  await expect(picked.locator('.nd-nb-dup')).toHaveText('+3 with this address');
  const title = await picked.locator('.nd-nb-dup').getAttribute('title');
  expect(title).toContain('wan-rtr-01 — link up');
  expect(title).toContain('wan-rtr-02 — link down');
  expect(title).toContain('1 more');
  // The badge carries no hover sentence any more (ADR-200 Inc.20): what the peer was matched on is
  // a line in the opened row, checked below.
  await expect(picked.locator('.nd-nb-state')).not.toHaveAttribute('title', /.+/);

  const ambiguous = row(page, 'Gi0/23');
  await expect(ambiguous.locator('.nd-nb-state')).toHaveText('Several nodes');
  await expect(ambiguous.locator('.nd-nb-link')).toHaveCount(0);
  // No node was picked, so the two are every claimant — "+2" would read as three nodes.
  await expect(ambiguous.locator('.nd-nb-dup')).toHaveText('2 nodes with this address');
});

test('the opened row lists the other nodes, linked, with their link state', async ({ page }) => {
  await openTab(page);
  await row(page, 'Gi0/24').locator('.nd-nb-proto').click();
  const also = page.locator('.nd-nb-also');
  await expect(also).toBeVisible();
  await expect(also.locator('.nd-nb-also-head')).toHaveText('3 other nodes have this address');
  const items = also.locator('.nd-nb-also-list li');
  await expect(items).toHaveCount(2);
  await expect(items.nth(0).locator('a')).toHaveText('wan-rtr-01');
  await expect(items.nth(0).locator('a')).toHaveAttribute(
    'href',
    /00000000-0000-4000-8000-000000000102/,
  );
  await expect(items.nth(0).locator('.nd-nb-port')).toHaveText('link up');
  await expect(items.nth(1).locator('.nd-nb-port')).toHaveText('link down');
  await expect(also).toContainText('1 more not listed here');
  // Picked among the claimants by its name, said where it can be read without a mouse.
  await expect(page.locator('.nd-nb-matched dd')).toHaveText('Name the neighbor sent');
});

test('an address several nodes have and no name picks says nothing was matched', async ({ page }) => {
  await openTab(page);
  await row(page, 'Gi0/23').locator('.nd-nb-proto').click();
  await expect(page.locator('.nd-nb-also')).toBeVisible();
  await expect(page.locator('.nd-nb-matched')).toHaveCount(0);
});
