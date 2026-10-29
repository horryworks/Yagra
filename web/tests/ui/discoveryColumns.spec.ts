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
