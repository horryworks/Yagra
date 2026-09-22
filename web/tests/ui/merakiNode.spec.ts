// SPDX-License-Identifier: AGPL-3.0-only
// A Cisco Meraki MX node's Overview card (ADR-164 増分 13・14): the tiles, one row per WAN uplink
// with its state and rates, and the WAN traffic chart.
//
// WHY A BROWSER. `merakiUplinkLines`, `merakiUplinkState` and `merakiTrafficSeries` are
// unit-tested. What is not: that the card asks for the per-uplink metrics *with their rows*, lays
// one row out per uplink, reads each uplink's history back by its row key, and — the defect
// increment 14 exists for — that nothing on the card spills over its neighbour when the pane is
// narrower. That last one is geometry, which no Vitest can see (`testing.md`). This is also the only
// Tier1 spec that renders a Meraki node's Overview at all: the walk's node is an SNMP device.
//
// The bodies are the generated ones, patched (testing.md) — the values are invented.

import type { Page } from '@playwright/test';
import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const NODE_ID = '00000000-0000-4000-8000-0000000000ab';
/** The other MX of its warm-spare pair (ADR-164 決定 26). */
const PARTNER_ID = '00000000-0000-4000-8000-0000000000ac';

const merakiNodeWith = (pair: Schemas['MerakiPairView']): Json => {
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
    meraki_pair: pair,
  } as unknown as Json;
};

const partner = (node_state: Schemas['NodeState']): Schemas['MerakiPartnerView'] => ({
  name: 'mx-spare-test',
  role: 'spare',
  node_id: PARTNER_ID,
  node_state,
});

/** This primary is up and its spare is not: the site has lost its redundancy. */
const merakiNode = merakiNodeWith({
  role: 'primary',
  state: 'spare_down',
  partner: partner('unreachable'),
});

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
  // Auto VPN (ADR-164 決定 25): a spoke that reaches one of its two hubs.
  const vpn: Record<string, number> = { meraki_vpn_hubs_reachable: 1, meraki_vpn_hubs_unreachable: 1 };
  if (name in vpn) return { ...body, metric: name, node_id: NODE_ID, value: vpn[name] } as unknown as Json;
  return body as unknown as Json;
}

/** The stored history of each uplink's rates, by row (決定 27). WAN2 has none — it has no line —
 *  so the chart carries WAN1 and cellular only. */
function range(url: URL): Json {
  const parts = url.pathname.split('/');
  const name = decodeURIComponent(parts[parts.length - 2] ?? '');
  const body = defaultBodyFor(url.pathname) as unknown as Schemas['MetricRange'];
  const row = Number(url.searchParams.get('row'));
  const rates: Record<string, Record<number, number>> = {
    meraki_uplink_sent_bps: { 1: 5_000, 3: 100 },
    meraki_uplink_recv_bps: { 1: 10_000, 3: 200 },
  };
  const v = rates[name]?.[row];
  // Inside the chart's default window (the last hour), so the lines are drawn and not only named.
  const now = Math.floor(Date.now() / 1000);
  const points = v == null ? [] : [1_500, 900, 300].map((d) => ({ t: now - d, v }));
  return { ...body, metric: name, node_id: NODE_ID, points } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': merakiNode,
      '/api/v1/nodes/{node_id}/metrics/{metric}': (url) => metric(url),
      '/api/v1/nodes/{node_id}/metrics/{metric}/range': (url) => range(url),
    },
    // A spoke has no spoke count: the server answers 404 for a metric with no reading.
    failures: { [`/api/v1/nodes/${NODE_ID}/metrics/meraki_vpn_spokes_unreachable`]: 404 },
  },
});

const cardOf = (page: Page) =>
  page.locator('section', { has: page.locator('.nd-section-t', { hasText: 'Cisco Meraki' }) });

/**
 * Nothing on the card is laid out over something else, and nothing runs past its own box.
 *
 * This is the defect increment 14 exists for: a sentence in a tile built for one number ran into
 * the next tile, at the width the node pane actually has. Checked from the boxes the browser laid
 * out, never with `isVisible()` (which is true for text drawn over its neighbour). Returns how many
 * boxes it inspected, so a selector that stopped matching cannot pass as "nothing overlaps".
 */
async function layoutProblems(page: Page): Promise<{ inspected: number; problems: string[] }> {
  return cardOf(page).evaluate((card) => {
    const problems: string[] = [];
    const boxes = [...card.querySelectorAll<HTMLElement>('.nd-mk-tile')];
    const rects = boxes.map((b) => b.getBoundingClientRect());
    const label = (el: Element) => el.querySelector('.nd-mk-tile-label')?.textContent ?? '?';
    // A tile's content never overflows the tile.
    boxes.forEach((b) => {
      if (b.scrollWidth > b.clientWidth + 1) problems.push(`tile "${label(b)}" overflows by ${b.scrollWidth - b.clientWidth}px`);
    });
    // No two tiles share any area.
    for (let i = 0; i < rects.length; i++) {
      for (let j = i + 1; j < rects.length; j++) {
        const a = rects[i];
        const c = rects[j];
        const overlap = a.left < c.right - 1 && c.left < a.right - 1 && a.top < c.bottom - 1 && c.top < a.bottom - 1;
        if (overlap) problems.push(`tiles "${label(boxes[i])}" and "${label(boxes[j])}" overlap`);
      }
    }
    // The uplink table fits its container; a cut name is allowed only with a title carrying it.
    const table = card.querySelector<HTMLElement>('.nd-mk-uplinks');
    if (table && table.scrollWidth > table.clientWidth + 1) problems.push(`uplink table overflows by ${table.scrollWidth - table.clientWidth}px`);
    for (const cell of card.querySelectorAll<HTMLElement>('.nd-mk-uplink > span')) {
      if (cell.scrollWidth > cell.clientWidth + 1 && cell.title !== cell.textContent) {
        problems.push(`uplink cell "${cell.textContent}" is cut off with no title`);
      }
    }
    // And the card as a whole stays inside its section.
    if (card.scrollWidth > card.clientWidth + 1) problems.push(`card overflows by ${card.scrollWidth - card.clientWidth}px`);
    return { inspected: boxes.length + (table ? 1 : 0), problems };
  });
}

test("an MX's card shows its tiles, a row per WAN uplink, and the WAN traffic chart", async ({
  page,
  mock,
  errors,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=overview`);
  const card = cardOf(page);
  await expect(card).toBeVisible({ timeout: 15_000 });

  // The header's kind badge wears Meraki's own green with white text (`lib/brandBadge.ts`) — read
  // from the laid-out page, because a rule that lost to the default accent still has the class.
  const badge = page.locator('.nd-kind', { hasText: /^Meraki$/ });
  await expect(badge).toHaveCSS('background-color', 'rgb(103, 179, 70)');
  await expect(badge).toHaveCSS('color', 'rgb(255, 255, 255)');

  const tile = (label: string) =>
    card.locator('.nd-mk-tile', {
      has: page.locator('.nd-mk-tile-label', { hasText: new RegExp(`^${label}$`) }),
    });
  await expect(card.locator('.nd-mk-tiles .nd-mk-tile-label')).toHaveText([
    'Availability',
    'Auto VPN',
    'Warm spare',
  ]);
  await expect(tile('Availability').locator('.nd-mk-tile-value')).toHaveText('Online');
  // One of its two hubs lost: the redundancy is gone, so the line reads as a warning (ADR-164 決定 25).
  const vpnValue = tile('Auto VPN').locator('.nd-mk-tile-value');
  await expect(vpnValue).toHaveText('1 of 2 hubs reachable');
  await expect(vpnValue).toHaveAttribute('style', /var\(--status-warning\)/);
  // The pair (ADR-164 決定 26): the state the server worked out, this MX's role, and its partner —
  // linked to the partner's own pane, because it is a node here.
  const pairValue = tile('Warm spare').locator('.nd-mk-tile-value');
  await expect(pairValue).toHaveText('Spare down');
  await expect(pairValue).toHaveAttribute('style', /var\(--status-warning\)/);
  // Two lines, each saying whose role it is — and that the role is the configured one, not which
  // MX carries traffic (that is the state above).
  const pairSub = tile('Warm spare').locator('.nd-mk-tile-sub');
  await expect(pairSub).toHaveText(['This MX: Primary (configured)', 'Partner: mx-spare-test (Spare)']);
  await expect(pairSub.locator('a')).toHaveAttribute('href', new RegExp(PARTNER_ID));

  // One row per uplink: its name, state and both rates. A port with no line has no rates; a failed
  // one is drawn as failed, not as "no line" (ADR-164 決定 24).
  const rows = card.locator('.nd-mk-uplink:not(.nd-mk-uplink-head)');
  await expect(rows).toHaveCount(3);
  await expect(rows.nth(0).locator('span')).toHaveText(['WAN1', 'Active', '5.0 kbps', '10.0 kbps']);
  await expect(rows.nth(1).locator('span')).toHaveText(['WAN2', 'No line', '—', '—']);
  await expect(rows.nth(2).locator('span')).toHaveText(['cellular', 'Failed', '0 bps', '0 bps']);
  await expect(rows.nth(2).locator('span').nth(1)).toHaveAttribute('style', /var\(--status-critical\)/);

  // The chart: each uplink that has history is two series, sent above and received below, plus
  // uPlot's x-axis row. WAN2 stored nothing, so it draws nothing rather than a flat zero.
  const legend = card.locator('.nd-mk-chart .u-legend .u-series');
  await expect(legend).toHaveCount(5);
  await expect(legend.locator('.u-label')).toHaveText([
    'Time',
    'WAN1 sent',
    'WAN1 received',
    'cellular sent',
    'cellular received',
  ]);
  // It read each uplink's history back by its row key — without `row` the server would answer the
  // node's one aggregate series, and every uplink would draw the same line.
  const ranged = mock.requests
    .filter((r) => r.pathname.startsWith(`/api/v1/nodes/${NODE_ID}/metrics/meraki_uplink_`) && r.pathname.endsWith('/range'))
    .map((r) => `${r.pathname.split('/').slice(-2, -1)[0]}@${new URLSearchParams(r.search).get('row')}`);
  for (const m of ['meraki_uplink_sent_bps', 'meraki_uplink_recv_bps']) {
    for (const row of ['1', '2', '3']) expect(ranged, `${m} row ${row}`).toContain(`${m}@${row}`);
  }

  // The card asked for the rows — without `rows=true` the server answers one number per metric.
  const asked = mock.requests
    .filter((r) => r.pathname.startsWith(`/api/v1/nodes/${NODE_ID}/metrics/meraki_uplink_`) && !r.pathname.endsWith('/range'))
    .map((r) => `${r.pathname.split('/').pop()}${r.search}`);
  for (const m of Object.keys(ROWS)) {
    expect(asked.some((a) => a.startsWith(m) && a.includes('rows=true')), m).toBe(true);
  }
  expect(errors.uncaught).toEqual([]);
});

// 🚨 The screenshot that started increment 14 was the node pane at about 850px: the uplink text
// ran over the next tile and the labels broke in two. Each width here is a real layout of the
// Overview — the whole-page detail at a laptop's width and two narrower desktop widths (the mobile
// layout starts at 768px and is not this card's question).
for (const width of [1280, 1000, 800]) {
  test(`nothing on the MX card runs over its neighbour at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 1000 });
    await page.goto(`/nodes/${NODE_ID}?tab=overview`);
    const card = cardOf(page);
    await expect(card.locator('.nd-mk-chart .u-legend .u-series')).toHaveCount(5, { timeout: 15_000 });
    const { inspected, problems } = await layoutProblems(page);
    expect(inspected, 'the check found the tiles and the table it inspects').toBeGreaterThanOrEqual(4);
    expect(problems).toEqual([]);
  });
}

test.describe('a primary that is down while its spare carries the site', () => {
  // The collector reads a pair's VPN row on the primary's serial, and only while that device is
  // online (ADR-164 決定 25), so nothing current exists — the card has to say the VPN is not
  // readable rather than draw no line, which would read as "no VPN here".
  //
  // The VPN reading IS served here, and that is the point: the latest-value read looks back 30
  // minutes, so on a lab deployment the primary's card kept "2 of 2 hubs reachable" from before the
  // failover beside "Running on spare". The card must not show it.
  const quiet = [
    'meraki_uplink_sent_bps',
    'meraki_uplink_recv_bps',
    'meraki_uplink_status',
    'meraki_vpn_spokes_unreachable',
  ];
  const stale: Record<string, number> = { meraki_vpn_hubs_reachable: 2, meraki_vpn_hubs_unreachable: 0 };
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/{node_id}': merakiNodeWith({
          role: 'primary',
          state: 'running_on_spare',
          partner: partner('ok'),
        }),
        '/api/v1/nodes/{node_id}/metrics/{metric}': (url) => {
          const name = decodeURIComponent(url.pathname.split('/').pop() ?? '');
          const body = defaultBodyFor(url.pathname) as unknown as Schemas['MetricReading'];
          return { ...body, metric: name, node_id: NODE_ID, value: stale[name] ?? 0 } as unknown as Json;
        },
      },
      failures: Object.fromEntries(
        quiet.map((m) => [`/api/v1/nodes/${NODE_ID}/metrics/${m}`, 404]),
      ),
    },
  });

  test('the card says the site runs on its spare, and why it has no VPN reading', async ({
    page,
    errors,
  }) => {
    await page.goto(`/nodes/${NODE_ID}?tab=overview`);
    const card = cardOf(page);
    await expect(card).toBeVisible({ timeout: 15_000 });
    const tile = (label: string) =>
      card.locator('.nd-mk-tile', {
        has: page.locator('.nd-mk-tile-label', { hasText: new RegExp(`^${label}$`) }),
      });

    await expect(tile('Warm spare').locator('.nd-mk-tile-value')).toHaveText('Running on spare');
    await expect(tile('Warm spare').locator('.nd-mk-tile-value')).toHaveAttribute(
      'style',
      /var\(--status-warning\)/,
    );
    await expect(tile('Auto VPN').locator('.nd-mk-tile-sub')).toHaveText(
      'Not read while the site runs on its spare',
    );
    await expect(tile('Auto VPN').locator('.nd-mk-tile-value')).toHaveText('—');
    await expect(card).not.toContainText('hubs reachable');
    await expect(card.locator('.nd-mk-tiles .nd-mk-tile-label')).toHaveText([
      'Availability',
      'Auto VPN',
      'Warm spare',
      'WAN traffic',
    ]);
    // No uplink reported, so there is no table and nothing to chart.
    await expect(card.locator('.nd-mk-uplinks')).toHaveCount(0);
    await expect(card.locator('.nd-mk-chart')).toHaveCount(0);
    expect(errors.uncaught).toEqual([]);
  });
});
