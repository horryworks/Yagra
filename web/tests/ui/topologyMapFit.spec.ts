// SPDX-License-Identifier: AGPL-3.0-only
// A big level of the network map fits its pane, and pressing a bundle of access points in a
// folder's pane lists them instead of opening the switch above it (ADR-191 Inc.14).
//
// Why Tier1: the arithmetic is unit-tested (`graphLayout.test.ts` wraps a wide rank and stacks
// islands, `fitView.test.ts` lowers the floor). What Vitest cannot run is the component that reads
// the pane's size and applies the fit, and the folder pane's press handler. The walk cannot stand in
// either: `bootstrap.ts` serves a level of one node and one folder, which fit at any floor.
//
// The level is the generated `MapLevel`, patched — a hand-written one would be a second copy of the
// contract (testing.md).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const FOLDER_ID = '00000000-0000-4000-8000-00000000c000';
const SWITCHES = 40;
const APS = 3;
const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const ROUTER = id(1);
const sw = (i: number) => id(1000 + i);
const ap = (i: number, j: number) => id(100_000 + i * 10 + j);

/** A router with forty switches under it, three access points on each: the shape that used to be
 *  one strip some fourteen thousand pixels wide. */
function level(url: URL): Json {
  const body = defaultBodyFor('/api/v1/topology/map') as Record<string, unknown>;
  const [nodeT] = body.nodes as Record<string, unknown>[];
  const [edgeT] = body.edges as Record<string, unknown>[];
  const node = (nid: string, name: string, role: string, accessPoint: boolean) => ({
    ...nodeT,
    id: nid,
    name: `${MOCK_PREFIX}${name}`,
    state: 'ok',
    role,
    access_point: accessPoint,
    folder_path: [],
    root_cause: null,
  });
  const edge = (a: string, b: string) => ({
    ...edgeT,
    id: `node:${a}|node:${b}`,
    a: { kind: 'node', id: a },
    b: { kind: 'node', id: b },
    source: 'lldp',
    sources: ['lldp'],
    count: 1,
    members: [],
  });
  const nodes = [node(ROUTER, 'router', 'edge', false)];
  const edges = [];
  for (let i = 0; i < SWITCHES; i++) {
    nodes.push(node(sw(i), `sw-${String(i).padStart(2, '0')}`, 'l2_switch', false));
    edges.push(edge(ROUTER, sw(i)));
    for (let j = 0; j < APS; j++) {
      nodes.push(node(ap(i, j), `ap-${i}-${j}`, 'access_point', true));
      edges.push(edge(sw(i), ap(i, j)));
    }
  }
  const group = url.searchParams.get('group');
  return {
    ...body,
    group: group ? { id: group, name: `${MOCK_PREFIX}big-site` } : null,
    overflow: false,
    nodes,
    edges,
    folders: [],
    stubs: [],
  } as unknown as Json;
}

function folders(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  return [
    { ...template, id: FOLDER_ID, name: `${MOCK_PREFIX}big-site`, parent_id: null, group_type: 'generic' },
  ] as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/topology/map': level,
      '/api/v1/node-groups': folders(),
    },
  },
});

/** Every drawn box lies inside the map's pane. */
async function expectAllInside(page: import('@playwright/test').Page, pane: string) {
  const box = (await page.locator(pane).boundingBox())!;
  const outside = await page.locator(`${pane} .topomap-node`).evaluateAll(
    (els, b) =>
      els
        .map((el) => el.getBoundingClientRect())
        .filter((r) => r.left < b.x - 1 || r.right > b.x + b.width + 1 || r.top < b.y - 1 || r.bottom > b.y + b.height + 1)
        .length,
    box,
  );
  expect(outside, 'boxes drawn outside the pane after a fit').toBe(0);
}

test('a level of forty switches fits the full map, and Fit fits it again after zooming in', async ({
  page,
  errors,
}) => {
  await page.goto(`/topology/map?group=${FOLDER_ID}`);
  const nodes = page.locator('.topomap .topomap-node');
  await expect(nodes).toHaveCount(1 + SWITCHES + SWITCHES); // router, switches, one bundle each
  await expectAllInside(page, '.topomap');

  await page.getByRole('button', { name: 'Zoom in' }).click();
  await page.getByRole('button', { name: 'Zoom in' }).click();
  await page.getByRole('button', { name: 'Fit to view' }).click();
  await expectAllInside(page, '.topomap');

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});

test("pressing a bundle in a folder's map lists its access points and stays on the folder", async ({
  page,
  errors,
}) => {
  await page.goto(`/nodes?sel=group:${FOLDER_ID}`);
  const map = page.locator('.nd-grpmap');
  const bundle = map.locator('.topomap-bundle').first();
  await expect(bundle).toBeVisible();
  await bundle.click();

  const heading = page.getByRole('heading', { name: /^Wi-Fi access points under / });
  await expect(heading).toBeVisible();
  await expect(page.locator('.nd-grpmap-edge .topomap-panel-ap')).toHaveCount(APS);
  // Still the folder: the press did not select the switch above the bundle in the tree.
  expect(new URL(page.url()).searchParams.get('sel')).toBe(`group:${FOLDER_ID}`);

  // A second press lets it go (ADR-073).
  await bundle.click();
  await expect(heading).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
  expect(errors.logged).toEqual([]);
});
