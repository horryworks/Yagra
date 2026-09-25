// SPDX-License-Identifier: AGPL-3.0-only
// The "Needs attention" preset on the inventory tree (ADR-163).
//
// Why Tier1: what the preset *selects* is decided in `inventoryFilters.ts` and unit-tested there.
// What only a browser proves is that the two surfaces are the same control — a switch behind the
// filter button (ADR-177) and the header's own "N need attention" count both writing the `state` column, and reading
// their pressed look back out of it. Nothing in Vitest runs either of those `.tsx` sites, and the
// failure they guard against is silent: a button that lights up while the tree behind it is
// unchanged, or a count that narrows a tree the operator cannot see.
//
// Its own fixture rather than `twoLevelTree`: the point needs nodes in DIFFERENT states, so that a
// tree narrowed to the attention states is visibly smaller than the tree beside it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';
import {
  filterChips,
  filterPopover,
  filterTrigger,
  inventorySwitch,
  openInventoryFilter,
  pressInventorySwitch,
} from './inventoryFilter';

type Page = import('@playwright/test').Page;

const label = (s: string) => `${MOCK_PREFIX}${s}`;

/** Three ungrouped nodes: one healthy, one warning, one down. `unreachable` is deliberately one of
 *  them — it is the state a down device actually carries, and the preset would be worth little if
 *  it dropped the rows an operator opens this screen for. */
const FLEET = [
  { id: '00000000-0000-4000-8000-0000000000d1', name: 'healthy-sw', state: 'ok' },
  { id: '00000000-0000-4000-8000-0000000000d2', name: 'hot-sw', state: 'warning' },
  { id: '00000000-0000-4000-8000-0000000000d3', name: 'down-sw', state: 'unreachable' },
] as const;

function node(n: (typeof FLEET)[number]): Record<string, Json> {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as { nodes: Record<string, Json>[] };
  return { ...body.nodes[0], id: n.id, name: label(n.name), group_id: null, state: n.state };
}

/** Browsing: every node sits at the root, so the unfiltered tree shows all three. */
function byGroup(url: URL): Json {
  const batch = url.searchParams.get('groups');
  if (batch) {
    return {
      nodes: [],
      truncated: false,
      answered: batch.split(',').filter(Boolean),
    } as unknown as Json;
  }
  if (url.searchParams.get('group')) return { nodes: [], truncated: false } as unknown as Json;
  return {
    nodes: FLEET.map((n, i) => ({ ...node(n), sort_order: i + 1 })),
  } as unknown as Json;
}

/** Filter mode: the tree stops reading `/nodes/by-group` and reads this instead, so the mock has to
 *  honour `state` for the press to change anything on screen.
 *
 *  🚨 **A mock that ignored the parameter would pass every assertion below except the ones about
 *  rows** — which is precisely why those are here: the button's `aria-pressed` comes from the URL
 *  and would look right over an unfiltered tree. */
function nodesPage(url: URL): Json {
  const want = new Set((url.searchParams.get('state') ?? '').split(',').filter(Boolean));
  const kept = want.size === 0 ? [...FLEET] : FLEET.filter((n) => want.has(n.state));
  return { nodes: kept.map(node), truncated: false, next_cursor: null } as unknown as Json;
}

/** The header count the preset is named after. It must be non-zero, or the button it becomes is
 *  never rendered. Two of the three nodes are in an attention state. */
function fleetSummary(): Json {
  return {
    total: 3,
    states: { ok: 1, warning: 1, critical: 0, unreachable: 1, unknown: 0, maintenance: 0 },
  } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': [] as unknown as Json,
      '/api/v1/nodes/by-group': byGroup,
      '/api/v1/nodes': nodesPage,
      '/api/v1/fleet/summary': fleetSummary(),
      '/api/v1/fleet/group-summary': { groups: {} } as unknown as Json,
    },
  },
});

const row = (page: Page, name: string) =>
  page.locator('.ntree-body').getByText(label(name), { exact: true });
// Behind the inventory's one filter button since ADR-177.
const toggle = (page: Page) => inventorySwitch(page, 'Needs attention');
const pressToggle = (page: Page) => pressInventorySwitch(page, 'Needs attention');
const headerCount = (page: Page) => page.getByRole('button', { name: /need attention/ });

/** Every node on screen, before anything is pressed — so a later "gone" can only be the preset. */
async function browsed(page: Page) {
  for (const n of FLEET) await expect(row(page, n.name)).toHaveCount(1);
}

test('the toggle keeps the warning and the down node and drops the healthy one', async ({
  page,
}) => {
  await page.goto('/nodes');
  await browsed(page);

  await pressToggle(page);
  await expect(toggle(page)).toBeChecked();
  await expect(row(page, 'healthy-sw'), 'a healthy node survived the preset').toHaveCount(0);
  await expect(row(page, 'hot-sw')).toHaveCount(1);
  // The reason the set is three states and not two: a down device is `unreachable`, never
  // `critical`, so a preset written from the severity words alone would hide it.
  await expect(row(page, 'down-sw'), 'the down node is the one this screen is opened for').toHaveCount(1);

  await pressToggle(page);
  await expect(toggle(page)).not.toBeChecked();
  await expect(row(page, 'healthy-sw')).toHaveCount(1);
});

test('it writes the state filter the operator can already see, rather than a hidden switch', async ({
  page,
}) => {
  await page.goto('/nodes');
  await pressToggle(page);

  // The URL is the whole of the preset's state (ADR-163 決定 5) — there is no preference behind it.
  await expect(page).toHaveURL(/[?&]state=warning%2Ccritical%2Cunreachable/);
  // And the State boxes in the same popover say so too. This is what makes the press explainable:
  // the operator can see WHICH states were chosen, and can take one back out by hand.
  const pop = filterPopover(page);
  for (const s of ['Warning', 'Critical', 'Unreachable']) {
    await expect(pop.getByRole('checkbox', { name: s, exact: true }), s).toBeChecked();
  }
  await expect(pop.getByRole('checkbox', { name: 'Ok', exact: true })).not.toBeChecked();
  // …while the chip row says it once, not twice (ADR-177 決定 3).
  await page.keyboard.press('Escape');
  await expect(filterChips(page).locator('.invf-chip')).toHaveCount(1);
  await expect(filterChips(page).getByText('Needs attention', { exact: true })).toHaveCount(1);
});

test('the header count presses the same preset, and the two agree', async ({ page }) => {
  await page.goto('/nodes');
  await expect(headerCount(page), 'the header count is the second way in').toHaveCount(1);
  await expect(headerCount(page)).toHaveAttribute('aria-pressed', 'false');

  await headerCount(page).click();
  // Both surfaces read their pressed look out of the same filter, so they cannot disagree — which
  // is the property worth pinning, since two controls over one state is exactly where they do.
  await expect(headerCount(page)).toHaveAttribute('aria-pressed', 'true');
  await openInventoryFilter(page);
  await expect(toggle(page)).toBeChecked();
  await expect(row(page, 'healthy-sw')).toHaveCount(0);

  await page.keyboard.press('Escape');
  await headerCount(page).click();
  await openInventoryFilter(page);
  await expect(toggle(page)).not.toBeChecked();
  await expect(row(page, 'healthy-sw')).toHaveCount(1);
});

test('"clear all filters" appears with it and switches it off', async ({ page }) => {
  await page.goto('/nodes');
  await pressToggle(page);
  await page.keyboard.press('Escape');
  const clear = page.getByRole('button', { name: /clear all filters/i });
  // No `extraActive` wiring exists for this button (ADR-163 決定 1) — it is counted because it
  // writes the `state` column. That is only true while it keeps writing it.
  await expect(clear, 'the tree is narrowed, so the reset has to be on screen').toHaveCount(1);
  await clear.click();
  await expect(row(page, 'healthy-sw')).toHaveCount(1);
  await openInventoryFilter(page);
  await expect(toggle(page)).not.toBeChecked();
});

test('pressing the header count opens the inventory when it is railed', async ({ page }) => {
  await page.goto('/nodes');
  await browsed(page);
  await page.getByRole('button', { name: 'Collapse inventory' }).click();
  await expect(page.locator('.nodes-pane.nodes-rail')).toHaveCount(1);

  // The count stays on screen while the tree is a 40px strip, so narrowing from there would be a
  // button that visibly does nothing (ADR-055 R6).
  await headerCount(page).click();
  await expect(page.locator('.nodes-pane.nodes-rail'), 'the pane stayed railed').toHaveCount(0);
  await expect(row(page, 'healthy-sw')).toHaveCount(0);
  await expect(row(page, 'down-sw')).toHaveCount(1);
});

test('the pane head keeps the filter button inside the pane at the narrowest tree', async ({
  page,
}) => {
  // ADR-177 put a fifth control in the 38px head, and `.nodes-pane` is `overflow: hidden`: a
  // button pushed past its right edge is not drawn and not pressable, and nothing else would fail.
  // 220px is `TREE_MIN_PX`; the search box is the part that gives.
  await page.goto('/nodes');
  await browsed(page);
  await page.getByRole('slider', { name: /resize the inventory pane/i }).focus();
  for (let i = 0; i < 10; i++) await page.keyboard.press('ArrowLeft');
  const pane = await page.locator('.nodes-pane').first().boundingBox();
  const btn = await filterTrigger(page).boundingBox();
  expect(pane, 'the pane has no box').not.toBeNull();
  expect(btn, 'the filter button has no box').not.toBeNull();
  expect(pane!.width, 'the pane did not reach its floor').toBeLessThanOrEqual(230);
  expect(btn!.x + btn!.width, 'the filter button is cut off by the pane').toBeLessThanOrEqual(
    pane!.x + pane!.width,
  );
});

test('the chip row still leaves the tree room to draw at 1280×360', async ({ page }) => {
  // ADR-159 決定 11's cap still binds the row under the head, now that it holds chips (ADR-177):
  // it wraps, is `flex: none` above a `flex: 1` tree, and once left `.ntree-body` at 0px.
  await page.setViewportSize({ width: 1280, height: 360 });
  await page.goto('/nodes');
  await browsed(page);
  await pressToggle(page);
  await pressInventorySwitch(page, 'Hide empty folders');
  // click, not check(): the box is controlled by the URL, which the router writes a frame later,
  // so check() reads it back before the write lands and reports that nothing changed.
  const device = filterPopover(page).getByRole('checkbox', { name: 'Device (ICMP / SNMP)', exact: true });
  await device.click();
  await expect(device).toBeChecked();
  // The popover is taller than this window, so it must scroll rather than run off the bottom:
  // its Done button is the last thing in it.
  await filterPopover(page).getByRole('button', { name: 'Done' }).scrollIntoViewIfNeeded();
  const done = await filterPopover(page).getByRole('button', { name: 'Done' }).boundingBox();
  expect(done!.y + done!.height, 'Done is below the bottom of the window').toBeLessThanOrEqual(360);
  await page.keyboard.press('Escape');
  await expect(filterChips(page)).toHaveCount(1);
  const tree = await page
    .locator('.ntree-body')
    .evaluate((el) => Math.round(el.getBoundingClientRect().height));
  expect(tree, 'the chip row has eaten the tree').toBeGreaterThan(48);
});
