// SPDX-License-Identifier: AGPL-3.0-only
// The "Needs attention" preset on the inventory tree (ADR-163).
//
// Why Tier1: what the preset *selects* is decided in `inventoryFilters.ts` and unit-tested there.
// What only a browser proves is that the two surfaces are the same control — a toggle in the filter
// row and the header's own "N need attention" count both writing the `state` column, and reading
// their pressed look back out of it. Nothing in Vitest runs either of those `.tsx` sites, and the
// failure they guard against is silent: a button that lights up while the tree behind it is
// unchanged, or a count that narrows a tree the operator cannot see.
//
// Its own fixture rather than `twoLevelTree`: the point needs nodes in DIFFERENT states, so that a
// tree narrowed to the attention states is visibly smaller than the tree beside it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

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
const toggle = (page: Page) => page.getByRole('button', { name: 'Needs attention', exact: true });
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

  await toggle(page).click();
  await expect(toggle(page)).toHaveAttribute('aria-pressed', 'true');
  await expect(row(page, 'healthy-sw'), 'a healthy node survived the preset').toHaveCount(0);
  await expect(row(page, 'hot-sw')).toHaveCount(1);
  // The reason the set is three states and not two: a down device is `unreachable`, never
  // `critical`, so a preset written from the severity words alone would hide it.
  await expect(row(page, 'down-sw'), 'the down node is the one this screen is opened for').toHaveCount(1);

  await toggle(page).click();
  await expect(toggle(page)).toHaveAttribute('aria-pressed', 'false');
  await expect(row(page, 'healthy-sw')).toHaveCount(1);
});

test('it writes the state filter the operator can already see, rather than a hidden switch', async ({
  page,
}) => {
  await page.goto('/nodes');
  await toggle(page).click();

  // The URL is the whole of the preset's state (ADR-163 決定 5) — there is no preference behind it.
  await expect(page).toHaveURL(/[?&]state=warning%2Ccritical%2Cunreachable/);
  // And the State control says so too. This is what makes the press explainable: the operator can
  // see WHICH filter was set, and can take one state back out by hand.
  const stateTrigger = page.getByRole('button', { name: 'Filter by state' });
  await expect(stateTrigger).toHaveAttribute('aria-expanded', 'false');
  await expect(stateTrigger, 'the State control did not show the preset').not.toHaveText(/^Any$/);
});

test('the header count presses the same preset, and the two agree', async ({ page }) => {
  await page.goto('/nodes');
  await expect(headerCount(page), 'the header count is the second way in').toHaveCount(1);
  await expect(headerCount(page)).toHaveAttribute('aria-pressed', 'false');

  await headerCount(page).click();
  // Both surfaces read their pressed look out of the same filter, so they cannot disagree — which
  // is the property worth pinning, since two controls over one state is exactly where they do.
  await expect(headerCount(page)).toHaveAttribute('aria-pressed', 'true');
  await expect(toggle(page)).toHaveAttribute('aria-pressed', 'true');
  await expect(row(page, 'healthy-sw')).toHaveCount(0);

  await headerCount(page).click();
  await expect(toggle(page)).toHaveAttribute('aria-pressed', 'false');
  await expect(row(page, 'healthy-sw')).toHaveCount(1);
});

test('"clear all filters" appears with it and switches it off', async ({ page }) => {
  await page.goto('/nodes');
  await toggle(page).click();
  const clear = page.getByRole('button', { name: /clear all filters/i });
  // No `extraActive` wiring exists for this button (ADR-163 決定 1) — it is counted because it
  // writes the `state` column. That is only true while it keeps writing it.
  await expect(clear, 'the tree is narrowed, so the reset has to be on screen').toHaveCount(1);
  await clear.click();
  await expect(toggle(page)).toHaveAttribute('aria-pressed', 'false');
  await expect(row(page, 'healthy-sw')).toHaveCount(1);
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

/** `{ drawn }` is the visible height of the filter row, `{ wanted }` what its contents ask for, and
 *  `{ tree }` what is left for the inventory. The three together are the only way to tell "the row
 *  fits" from "the cap is hiding half of it". */
async function paneGeometry(page: Page) {
  const row = await page
    .locator('.nodes-pane-filters')
    .evaluate((el) => ({ drawn: el.clientHeight, wanted: el.scrollHeight }));
  const tree = await page
    .locator('.ntree-body')
    .evaluate((el) => Math.round(el.getBoundingClientRect().height));
  return { ...row, tree };
}

test('the filter row fits on an ordinary window, with nothing behind a scrollbar', async ({
  page,
}) => {
  // 🚨 ADR-159 決定 11 capped this row at half the pane so it could never zero the tree again. The
  // cap makes a row that does not fit **scroll**, which is a quiet failure: `ClearFilters` and the
  // State control are the last children, so they are the ones that go out of reach, and every
  // assertion in this file would still pass with them hidden.
  //
  // Measured at the walk's own viewport with the sixth control in place: 144px drawn, 144px wanted,
  // 357px left for the tree. ⚠️ **Shortening the label does not move that number** — `Attention`
  // (86px) was measured and gives the same 144px, because at a 310px pane the row fits two controls
  // per line either way. So ADR-159 決定 10's advice ("make the label short") does not generalize:
  // what costs a line here is the sixth control, not its width.
  await page.goto('/nodes');
  await browsed(page);
  const g = await paneGeometry(page);
  expect(g.wanted, 'the filter row now overflows on a normal window — controls are unreachable')
    .toBeLessThanOrEqual(g.drawn);
});

test('the filter row still leaves the tree room to draw at 1280×360', async ({ page }) => {
  // The other end of the same rule. This row wraps and is `flex: none` above a `flex: 1` tree, and
  // the third toggle once left `.ntree-body` at **0px** — five filter controls above an inventory
  // showing nothing. This is the sixth control, and here the cap IS binding (measured: 89px drawn
  // against 144px wanted), which is the trade 決定 11 chose: at this height the row scrolls so the
  // tree does not vanish.
  await page.setViewportSize({ width: 1280, height: 360 });
  await page.goto('/nodes');
  await browsed(page);

  const g = await paneGeometry(page);
  // Three rows is the bar ADR-159 measured itself against after the cap went in (49–52px there);
  // this control must not move that number.
  expect(g.tree, 'the filter row has eaten the tree again').toBeGreaterThan(48);
});
