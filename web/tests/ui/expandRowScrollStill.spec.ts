// SPDX-License-Identifier: AGPL-3.0-only
// Opening a row does not move the rows around it (ADR-203 item 8).
//
// Every table that opens a row allows one open row, so opening B closes A. When A had been scrolled
// out of the rendered window, the table forgot every measured height (`virtualizer.measure()`) and
// re-measured only what was mounted: A fell back to the 44px estimate with nothing compensating,
// and everything on screen — B included, under the pointer — moved up by A's extra height.
//
// Neighbors is the instance used here: its table scrolls inside a box of its own (`.nd-nb-table`),
// which is what lets a row leave the rendered window at all. On a page where the table grows to its
// content (Metric sets) every row stays mounted and the defect cannot happen.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import type { Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000ad';
const COUNT = 60;
const port = (i: number) => `Gi0/${i + 1}`;

function neighbors(): Json {
  return {
    first_seen: '2026-09-30T00:00:00Z',
    last_seen: '2026-09-30T00:00:00Z',
    neighbors: {
      truncated: false,
      neighbors: Array.from({ length: COUNT }, (_, i) => ({
        remote_port_desc: null,
        remote_sys_desc: null,
        remote_platform: null,
        capabilities: [],
        local_ifindex: null,
        proto: 'lldp',
        local_port: port(i),
        remote_chassis: `aa:bb:cc:00:00:${String(i).padStart(2, '0')}`,
        remote_chassis_kind: 'mac',
        remote_sys_name: `peer-${String(i).padStart(2, '0')}`,
        remote_port: 'eth1',
        remote_mgmt_addr: null,
      })),
    },
    peers: [],
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
      '/api/v1/nodes/{node_id}/neighbors': () => neighbors(),
    },
  },
});

type Page = import('@playwright/test').Page;
const rowOf = (page: Page, i: number) =>
  page.locator('.dt-row').filter({ hasText: `peer-${String(i).padStart(2, '0')}` }).first();

test('opening a row while another open row is scrolled away keeps the row under the pointer still', async ({
  page,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=neighbors`);
  await expect(rowOf(page, 0)).toBeVisible({ timeout: 15_000 });
  const closedHeight = (await page.locator('.nd-nb-table [data-index="0"]').boundingBox())!.height;
  await rowOf(page, 0).locator('.nd-nb-proto').click();
  await page.waitForTimeout(300);
  const openHeight = (await page.locator('.nd-nb-table [data-index="0"]').boundingBox())!.height;
  expect(openHeight - closedHeight, 'the open row is not taller — nothing here could shift').toBeGreaterThan(60);

  // Wheel far enough that row 0 leaves the rendered window (overscan is twelve rows).
  const scroller = page.locator('.nd-nb-table .dt-scroll');
  const room = await scroller.evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(room, 'the table does not scroll inside its box — this fixture cannot see the bug').toBeGreaterThan(1200);
  const box = (await scroller.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 60);
  for (let i = 0; i < 10; i++) await page.mouse.wheel(0, 300);
  await page.waitForTimeout(300);
  await expect(page.locator('.nd-nb-table [data-index="0"]'), 'row 0 is still mounted').toHaveCount(0);

  const target = await scroller.evaluate((el) => {
    const top = el.getBoundingClientRect().top + 40;
    const r = [...el.querySelectorAll<HTMLElement>('[data-index]')].find(
      (x) => x.getBoundingClientRect().top > top,
    );
    return r ? Number(r.dataset.index) : -1;
  });
  expect(target).toBeGreaterThan(14);
  const before = (await rowOf(page, target).boundingBox())!.y;
  await rowOf(page, target).locator('.nd-nb-proto').click();
  await page.waitForTimeout(400);
  const after = (await rowOf(page, target).boundingBox())!.y;
  expect(Math.abs(after - before), 'the row that was opened moved under the pointer').toBeLessThan(4);
});

test('a row that closed leaves no gap and no overlap behind it, scrolled away or not', async ({ page }) => {
  // What `measure()` was there for, kept true without it: a closed row must not keep its open height
  // (a gap), and a row must not keep the estimate while it is taller (an overlap, ADR-179 Inc.3).
  await page.goto(`/nodes/${NODE_ID}?tab=neighbors`);
  await expect(rowOf(page, 0)).toBeVisible({ timeout: 15_000 });
  await rowOf(page, 0).locator('.nd-nb-proto').click();
  await page.waitForTimeout(300);
  const scroller = page.locator('.nd-nb-table .dt-scroll');
  const box = (await scroller.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + 60);
  for (let i = 0; i < 10; i++) await page.mouse.wheel(0, 300);
  await page.waitForTimeout(300);
  // Open another, which closes row 0 while it is not rendered, then go back up to it.
  const target = await scroller.evaluate((el) => {
    const top = el.getBoundingClientRect().top + 40;
    const r = [...el.querySelectorAll<HTMLElement>('[data-index]')].find(
      (x) => x.getBoundingClientRect().top > top,
    );
    return r ? Number(r.dataset.index) : -1;
  });
  expect(target).toBeGreaterThan(14);
  await rowOf(page, target).locator('.nd-nb-proto').click();
  await page.waitForTimeout(300);
  for (let i = 0; i < 15; i++) await page.mouse.wheel(0, -300);
  await page.waitForTimeout(400);
  await expect(rowOf(page, 0)).toBeVisible();

  const layout = await scroller.evaluate((el) =>
    [...el.querySelectorAll<HTMLElement>('[data-index]')]
      .map((r) => ({ i: Number(r.dataset.index), top: r.getBoundingClientRect().top, h: r.getBoundingClientRect().height }))
      .sort((a, b) => a.i - b.i),
  );
  expect(layout.length).toBeGreaterThan(5);
  for (let k = 1; k < layout.length; k++) {
    const prev = layout[k - 1];
    const cur = layout[k];
    if (cur.i !== prev.i + 1) continue;
    expect(Math.abs(cur.top - (prev.top + prev.h)), `a gap or an overlap between rows ${prev.i} and ${cur.i}`).toBeLessThan(2);
  }
  const closed = layout.find((r) => r.i === 0)!;
  expect(closed.h, 'row 0 kept its open height after it closed').toBeLessThan(80);
});
