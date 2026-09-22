// SPDX-License-Identifier: AGPL-3.0-only
// A Cisco Meraki MX node's Overview card: one line per WAN uplink, with its state and its rates
// (ADR-164 増分 13).
//
// WHY A BROWSER. `merakiUplinkLines` and `merakiUplinkState` are unit-tested. What is not: that the
// card asks for the three per-uplink metrics *with their rows*, lays one line out per uplink, and
// words the state — the difference this increment exists for is "failed" beside "no line", and a
// card that dropped the state, or showed the row key where the uplink's name belongs, would pass
// every Vitest. This is also the first Tier1 spec that renders a Meraki node's Overview at all: the
// walk's node is an SNMP device.
//
// The bodies are the generated ones, patched (testing.md) — the values are invented.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const NODE_ID = '00000000-0000-4000-8000-0000000000ab';

const merakiNode = (() => {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as unknown as Schemas['NodeDetail'];
  return {
    ...body,
    id: NODE_ID,
    kind: 'meraki',
    snmp_configured: false,
    meraki_device: {
      serial: 'Q2XX-TEST-0001',
      product_type: 'appliance',
      model: 'MX85',
      network_id: 'N_1',
      org_id: '1',
      org_uuid: '00000000-0000-4000-8000-000000000ace',
    },
  } as unknown as Json;
})();

type Row = { row: number; name: string; value: number };
/** Per-uplink readings, as `rows=true` answers them. WAN2 is a port with no line; cellular failed. */
const ROWS: Record<string, Row[]> = {
  meraki_uplink_sent_bps: [
    { row: 1, name: 'WAN1', value: 5_000 },
    { row: 3, name: 'cellular', value: 0 },
  ],
  meraki_uplink_recv_bps: [
    { row: 1, name: 'WAN1', value: 10_000 },
    { row: 3, name: 'cellular', value: 0 },
  ],
  meraki_uplink_status: [
    { row: 1, name: 'WAN1', value: 2 },
    { row: 2, name: 'WAN2', value: 0 },
    { row: 3, name: 'cellular', value: -1 },
  ],
};

/** Every metric read on the page: the three per-uplink ones by row, availability up, the rest as
 *  generated. */
function metric(url: URL): Json {
  const name = decodeURIComponent(url.pathname.split('/').pop() ?? '');
  const body = defaultBodyFor(url.pathname) as unknown as Schemas['MetricReading'];
  const rows = ROWS[name];
  if (rows) {
    return {
      ...body,
      metric: name,
      node_id: NODE_ID,
      value: Math.max(...rows.map((r) => r.value)),
      rows,
    } as unknown as Json;
  }
  if (name === 'meraki_device_up') return { ...body, metric: name, node_id: NODE_ID, value: 1 } as unknown as Json;
  return body as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': merakiNode,
      '/api/v1/nodes/{node_id}/metrics/{metric}': (url) => metric(url),
    },
  },
});

test("an MX's card shows each WAN uplink with its state and rates — failed and no line apart", async ({
  page,
  mock,
  errors,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=overview`);
  const card = page.locator('section', {
    has: page.locator('.nd-section-t', { hasText: 'Cisco Meraki' }),
  });
  await expect(card).toBeVisible({ timeout: 15_000 });

  const line = (uplink: string) =>
    card.locator('.nd-health-metric-head', {
      has: page.locator('.nd-health-metric-label', { hasText: new RegExp(`^${uplink}$`) }),
    });
  await expect(line('WAN1').locator('.nd-health-metric-value')).toHaveText(
    'Active · sent 5.0 kbps / received 10.0 kbps',
  );
  // A port with no line: its state, and no rates it never had.
  await expect(line('WAN2').locator('.nd-health-metric-value')).toHaveText('No line');
  await expect(line('cellular').locator('.nd-health-metric-value')).toHaveText(
    'Failed · sent 0 bps / received 0 bps',
  );
  await expect(card.locator('.nd-health-metric-label')).toHaveText([
    'Availability',
    'WAN1',
    'WAN2',
    'cellular',
  ]);

  // The card asked for the rows — without `rows=true` the server answers one number per metric.
  const asked = mock.requests
    .filter((r) => r.pathname.startsWith(`/api/v1/nodes/${NODE_ID}/metrics/meraki_uplink_`))
    .map((r) => `${r.pathname.split('/').pop()}${r.search}`);
  for (const m of Object.keys(ROWS)) {
    expect(asked.some((a) => a.startsWith(m) && a.includes('rows=true')), m).toBe(true);
  }
  expect(errors.uncaught).toEqual([]);
});
