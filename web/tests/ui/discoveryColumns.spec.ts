// SPDX-License-Identifier: AGPL-3.0-only
// Discovery's "Discovered devices" table keeps every row's cells under their headers.
//
// WHY A BROWSER. The header, the filter row and each data row are separate grids sharing one
// template (`DiscoveryPage.css`). A bare `1fr` track grows to its own row's content, so a row whose
// folder picker carries a long option, or whose identity is a long unbroken string, laid its
// columns out wider than the header's and the table read as misaligned. Nothing but layout can see
// that; the route walk never fills the table with rows of different shapes.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const LONG = 'ymock-region / ymock-a-very-long-site-name-that-widens-the-folder-picker / floor-07';

const groups = (() => {
  const body = defaultBodyFor('/api/v1/node-groups') as unknown as Schemas['GroupSummary'][];
  return body.map((g) => ({ ...g, name: LONG })) as unknown as Json;
})();

const candidate = (address: string, extra: Partial<Schemas['Candidate']>): Schemas['Candidate'] => ({
  address,
  reachable: true,
  ...extra,
});

const scan = (url: URL): Json => {
  const base = defaultBodyFor('/api/v1/discovery/scan/{id}') as unknown as Schemas['ScanView'];
  return {
    ...base,
    scan_id: url.pathname.split('/').pop() ?? base.scan_id,
    state: 'done',
    done: true,
    candidates: [
      candidate('192.0.2.1', {
        sysname: 'sw-01',
        sysdescr: 'Ymock Network OS Software, Version 99.1.2.3-build-'.repeat(4),
        sysobjectid: '1.3.6.1.4.1.99999.1.2.3.4.5.6.7.8.9',
        vendor: 'Ymock',
        model: 'YM-9000-48X',
        matched_credential_id: '00000000-0000-4000-8000-0000000000c1',
      }),
      candidate('192.0.2.2', {}),
      candidate('192.0.2.3', { reachable: false }),
    ],
    existing: [],
    same_device: [],
  } as unknown as Json;
};

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': groups,
      '/api/v1/discovery/scan/{id}': scan,
    },
  },
});

test('every discovered row lays its cells out under the header columns', async ({ page, errors }) => {
  await page.goto('/nodes/discovery');
  const rows = page.locator('.disco-row');
  await expect(rows).toHaveCount(3);

  const head = await page
    .locator('.disco-head > *')
    .evaluateAll((els) => els.map((e) => Math.round(e.getBoundingClientRect().left)));
  expect(head).toHaveLength(7);
  for (let i = 0; i < 3; i += 1) {
    const cells = await rows
      .nth(i)
      .locator(':scope > *')
      .evaluateAll((els) =>
        els.map((e) => {
          // The checkbox's wrapper is `display: contents` on the desktop; the input is the cell.
          const box = getComputedStyle(e).display === 'contents' ? (e.firstElementChild ?? e) : e;
          const r = box.getBoundingClientRect();
          return [Math.round(r.left), Math.round(r.right)];
        }),
      );
    // The checkbox is centred in its track, so its own left edge is not the track's.
    expect(cells.slice(1).map(([l]) => l), `row ${i + 1}`).toEqual(head.slice(1));
    // And no cell reaches into the next column.
    for (let c = 0; c + 1 < cells.length; c += 1) {
      expect(cells[c][1], `row ${i + 1} cell ${c + 1}`).toBeLessThanOrEqual(head[c + 1]);
    }
  }
  expect(errors.uncaught).toEqual([]);
});

// ADR-053 Inc.12: the Credential column narrows to the devices SNMP answered, and its control sits
// under the Credential header like the other two.
test('the Credential filter keeps the devices SNMP answered', async ({ page, errors }) => {
  await page.goto('/nodes/discovery?candidates.credential=answered');
  const rows = page.locator('.disco-row');
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText('192.0.2.1');

  const cells = page.locator('.disco-filters > *');
  await expect(cells).toHaveCount(7);
  const heads = page.locator('.disco-head > *');
  const left = async (l: typeof cells) =>
    Math.round((await l.boundingBox())?.x ?? Number.NaN);
  expect(await left(cells.nth(6))).toBe(await left(heads.nth(6)));
  await expect(cells.nth(6).getByRole('button', { name: 'Filter by Credential' })).toContainText(
    'SNMP answered',
  );
  expect(errors.uncaught).toEqual([]);
});

// Unregistered devices: the same three-grids-one-template shape. Its last track was `auto`, which
// sizes to its own row's content (empty in the header, the Monitored filter in the filter row,
// Detect in a data row), so every row resolved the template to different widths and the columns
// shifted under their headers even with ordinary data.
test.describe('Unregistered devices', () => {
  type Row = Schemas['DiscoveredEndpointRow'];
  const page0 = defaultBodyFor('/api/v1/discovered-endpoints') as unknown as Schemas['DiscoveredEndpointPage'];
  const base = page0.endpoints[0];
  const row = (id: string, ip: string, extra: Partial<Row>): Row => ({
    ...base,
    id,
    ip,
    promoted_node_id: null,
    ...extra,
  });

  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/discovered-endpoints': {
          ...page0,
          endpoints: [
            row('00000000-0000-4000-8000-0000000000e1', '192.0.2.10', { name: null, mac: null }),
            row('00000000-0000-4000-8000-0000000000e2', '192.0.2.11', {
              name: 'ymock-a-very-long-lldp-system-name-that-runs-on-and-on-without-a-break',
            }),
          ],
        } as unknown as Json,
      },
    },
  });

  test('every row resolves the column template the header does', async ({ page, errors }) => {
    await page.goto('/nodes/discovery?tab=unregistered');
    await expect(page.locator('.disco-seen-row')).toHaveCount(2);
    const templates = await page
      .locator('.disco-seen-head, .disco-seen-filters, .disco-seen-row')
      .evaluateAll((els) => els.map((e) => getComputedStyle(e).gridTemplateColumns));
    // Head, filter row, two data rows.
    expect(templates).toHaveLength(4);
    for (const [i, tpl] of templates.entries()) {
      expect(tpl, `grid ${i + 1}`).toBe(templates[0]);
    }
    expect(errors.uncaught).toEqual([]);
  });
});
