// SPDX-License-Identifier: AGPL-3.0-only
// The node Overview's two metric sections, in a real browser (ADR-046 Inc.6).
//
// WHY THIS SCREEN. Two things changed and neither is reachable from Vitest.
//
// 1. `System (SNMP)` stopped being a `name: value` strip and became cards with charts. Whether a
//    card *draws* is a canvas question, and `metricCards.test.ts` can only prove the list it is
//    given is the right list.
// 2. `SETUP RATE` was drawing `huawei_usg_session_total` — a **counter** — as a raw range and
//    labelling its since-boot total "/s" (18,190,268/s on the real firewall). The unit test pins
//    the *decision*; only this file can prove the decision reached the request. That is the whole
//    failure mode: the wrong query produced a chart that looked entirely healthy, a smooth rising
//    line, for as long as the card existed.
//
// So the load-bearing assertions here are about the requests, not the pixels. A card that renders
// from the wrong query is the bug, not the absence of a card.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** Charted by Device health's CPU card, so it must NOT reappear below. */
const CURATED_GAUGE = 'huawei_cpu_usage';
/** Device health's `setupRate` first candidate — the counter that started this. */
const CURATED_COUNTER = 'huawei_usg_session_total';
/** Left over for the generic section: explained by the catalogue, per-entity. */
const GENERIC_EXPLAINED = 'huawei_temp';
/** Left over, and deliberately a name nothing explains — an operator's own collection item. */
const GENERIC_UNEXPLAINED = 'ymock_overview_widget';

function deviceNode(): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as { kind: string };
  body.kind = 'device';
  return body as unknown as Json;
}

/** A four-metric inventory spanning every case the two sections divide on.
 *
 *  ⚠️ Real metric names on purpose for the first three: `huawei_cpu_usage` has to be a real
 *  `METRIC_CARDS` candidate or the subtraction has nothing to subtract, and `huawei_temp` has to
 *  be a real catalogue entry or the meaning line is legitimately absent and proves nothing. */
function inventory(): Json {
  return [
    { metric: CURATED_GAUGE, metric_kind: 'gauge', dimension: 'entity', status: 'ok', series_count: 15 },
    { metric: CURATED_COUNTER, metric_kind: 'counter', dimension: 'none', status: 'ok', series_count: 1 },
    { metric: GENERIC_EXPLAINED, metric_kind: 'gauge', dimension: 'entity', status: 'ok', series_count: 15 },
    { metric: GENERIC_UNEXPLAINED, metric_kind: 'gauge', dimension: 'none', status: 'ok', series_count: 1 },
  ] as unknown as Json;
}

/** Points covering the window the client actually asked for.
 *
 *  ⚠️ The generated mock gives every array one element and every number `1`, so the single sample
 *  lands at Unix second 1 — an hour of 1970, outside any requested range. uPlot then has nothing
 *  in range and the card is indistinguishable from one with no history. Reading `from`/`to` off
 *  the request keeps this true if `RangeControl`'s default ever moves. */
function rangeBody(url: URL): Json {
  const from = Number(url.searchParams.get('from'));
  const to = Number(url.searchParams.get('to'));
  const n = 12;
  return {
    node_id: NODE_ID,
    metric: 'mock',
    points: Array.from({ length: n }, (_, i) => ({
      t: Math.round(from + ((to - from) * i) / (n - 1)),
      v: (i + 1) * 3,
    })),
  } as unknown as Json;
}

function readingBody(): Json {
  return { node_id: NODE_ID, metric: 'mock', value: 42 } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(),
      '/api/v1/nodes/{node_id}/metrics': () => inventory(),
      '/api/v1/nodes/{node_id}/metrics/{metric}': () => readingBody(),
      '/api/v1/nodes/{node_id}/metrics/{metric}/range': (url: URL) => rangeBody(url),
    },
  },
});

/** The generic section, located by its own note rather than by DOM order — Device health uses the
 *  same grid class, so `.nd-health-metrics` alone would match either one. */
function genericSection(page: import('@playwright/test').Page) {
  return page.locator('section', { has: page.locator('.nd-section-note') });
}

async function openOverview(page: import('@playwright/test').Page) {
  await page.goto(`/nodes/${NODE_ID}?tab=overview`);
  await expect(page.getByRole('tab').first()).toBeVisible({ timeout: 15_000 });
  await expect(genericSection(page).locator('.nd-health-metric').first()).toBeVisible({
    timeout: 15_000,
  });
}

test.describe('node Overview metric cards', () => {
  test('draws a chart for every node-level metric, not a value strip', async ({ page, errors }) => {
    await openOverview(page);
    const cards = genericSection(page).locator('.nd-health-metric');
    await expect(cards).toHaveCount(2);
    // A canvas per card is the change. Before Inc.6 these rows had a number and nothing else, and
    // `.nd-muted` ("No history yet…") is what a card with an unusable series renders instead —
    // which is what the 1970 sample in the generated mock would produce.
    await expect(cards.locator('canvas')).toHaveCount(2);
    await expect(genericSection(page).locator('.nd-muted')).toHaveCount(0);
    expect(errors.uncaught).toEqual([]);
    expect(errors.logged).toEqual([]);
  });

  test('does not repeat a metric Device health is already charting', async ({ page }) => {
    await openOverview(page);
    const generic = genericSection(page);
    await expect(generic.getByText(GENERIC_EXPLAINED, { exact: true })).toBeVisible();
    await expect(generic.getByText(GENERIC_UNEXPLAINED, { exact: true })).toBeVisible();
    // The CPU gauge is charted above under its curated name; a second chart of it below reads as a
    // different measurement that happens to always agree.
    await expect(generic.getByText(CURATED_GAUGE, { exact: true })).toHaveCount(0);
    // ...and the whole page shows it once, under the curated label rather than the raw name.
    await expect(page.getByText(CURATED_GAUGE, { exact: true })).toHaveCount(0);
  });

  test('labels a generic card with its raw name and what it measures', async ({ page }) => {
    await openOverview(page);
    const explained = genericSection(page).locator('.nd-health-metric', {
      has: page.getByText(GENERIC_EXPLAINED, { exact: true }),
    });
    // Mono, because the label is an identifier — and un-uppercased, or a name this long wraps in a
    // 240px grid cell.
    const label = explained.locator('.nd-health-metric-label');
    await expect(label).toHaveClass(/mono/);
    expect(await label.evaluate((el) => getComputedStyle(el).textTransform)).toBe('none');
    // The catalogue sentence, which already exists in both locales for every explained gauge.
    await expect(explained.locator('.nd-health-metric-meaning')).not.toBeEmpty();
    // A metric nothing explains gets no line at all rather than invented prose.
    const unexplained = genericSection(page).locator('.nd-health-metric', {
      has: page.getByText(GENERIC_UNEXPLAINED, { exact: true }),
    });
    await expect(unexplained.locator('.nd-health-metric-meaning')).toHaveCount(0);
  });

  test('asks for a counter as a rate, and never asks for its stored value', async ({
    page,
    mock,
  }) => {
    await openOverview(page);
    const rangeOf = (metric: string) =>
      mock.requests.filter((r) => r.pathname.endsWith(`/metrics/${metric}/range`));
    const latestOf = (metric: string) =>
      mock.requests.filter((r) => r.pathname.endsWith(`/metrics/${metric}`));

    // 🚨 The regression. Without `rate=true` the endpoint returns the odometer's stored values and
    // the card draws a straight rising line captioned "/s" — a wrong answer that looks like a
    // working chart, which is why nothing caught it for the life of the card.
    const counterRange = rangeOf(CURATED_COUNTER);
    expect(counterRange.length).toBeGreaterThan(0);
    for (const r of counterRange) expect(r.search).toContain('rate=true');
    // The other half: a counter has no readable current value, so the card must not ask for one.
    // It used to, and that reading — 18,190,268 — was the headline.
    expect(latestOf(CURATED_COUNTER)).toEqual([]);

    // A per-entity gauge still collapses node-wide, and is never asked for as a rate.
    const gaugeRange = rangeOf(GENERIC_EXPLAINED);
    expect(gaugeRange.length).toBeGreaterThan(0);
    for (const r of gaugeRange) {
      expect(r.search).toContain('agg=max');
      expect(r.search).not.toContain('rate=true');
    }
    expect(latestOf(GENERIC_EXPLAINED).length).toBeGreaterThan(0);
  });
});
