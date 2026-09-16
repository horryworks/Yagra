// SPDX-License-Identifier: AGPL-3.0-only
// A filter an operator set is still set after a reload (ADR-153).
//
// Why Tier1: the codec and the hooks are unit-tested, but which key each screen passes — and whether
// a route with two or three tables gave each one its own prefix — is decided in `.tsx` files Vitest
// never runs. Each test here sets a filter through the real control, reloads from the URL the screen
// itself wrote (`page.goto(page.url())` — the whole of what an F5 keeps, since the harness re-seeds
// localStorage on every navigation), and checks the control still says so.
//
// The screens here are the ones that USED to lose their filters. The ones that were already
// URL-backed have their own round-trip tests (`columnFilter.spec.ts` for the Events log).

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, REPORT_TOOL, TREE_SIBLING_IDS } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Page = import('@playwright/test').Page;
type Locator = import('@playwright/test').Locator;

test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

const NEEDLE = 'edge';

/** Open a text column's filter under `scope`, type into it and commit.
 *
 *  Enter commits at once; the box otherwise commits on its settle, and closing the popover before
 *  that would unmount the draft with the term uncommitted — a test artefact, not a screen bug. */
async function typeFilter(page: Page, scope: Locator | Page, column: string, term: string) {
  await scope.getByRole('button', { name: `Filter by ${column}` }).click();
  const box = page.getByRole('dialog').getByRole('searchbox').first();
  await box.fill(term);
  await box.press('Enter');
  await page.keyboard.press('Escape');
  await expect(page.getByRole('dialog')).toHaveCount(0);
}

const reload = async (page: Page) => {
  await page.goto(page.url());
};

test.describe('Reports — three tables and a tab on one route', () => {
  test('the tab and its table filter survive a reload, under that table’s own key', async ({ page }) => {
    await page.goto('/dashboard/reports');
    await page.getByRole('tab', { name: /Schedules/ }).click();
    await expect(page, 'the tab never reached the URL').toHaveURL(/[?&]tab=schedules/);

    await typeFilter(page, page, 'Report', NEEDLE);
    await expect(page, 'the filter was not written under the schedules prefix').toHaveURL(
      new RegExp(`[?&]schedules\\.name=[^&]*${NEEDLE}`),
    );
    expect(new URL(page.url()).searchParams.has('name'), 'a bare `name` key was written').toBe(false);

    await reload(page);
    await expect(page.getByRole('tab', { name: /Schedules/ }), 'the reload fell back to the first tab').toHaveAttribute(
      'aria-selected',
      'true',
    );
    await expect(page.getByRole('button', { name: /Clear all filters/ })).toHaveCount(1);

    // The same column on another tab is a different table: it must not arrive filtered.
    await page.getByRole('tab', { name: /Saved reports/ }).click();
    await expect(page.getByRole('button', { name: /Clear all filters/ })).toHaveCount(0);
  });
});

test.describe('a node-detail tab', () => {
  /** Every node in the tree is an SNMP-polled device, so every one has an Interfaces tab. */
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/{node_id}': (url) => {
          const id = url.pathname.split('/').pop() ?? '';
          const body = defaultBodyFor(`/api/v1/nodes/${id}`) as { id: string; kind: string; snmp_configured: boolean };
          body.id = id;
          body.kind = 'device';
          body.snmp_configured = true;
          return body as unknown as Json;
        },
      },
    },
  });

  const pane = (page: Page) => page.locator('.nodes-detail-pane');

  test('shares the URL with the tree without reading or clearing the tree’s filter', async ({ page }) => {
    // The tree's `kind` and the Events tab's `kind` are one column name on one URL. Arrive with the
    // tree's set: a tab that read it bare would show as filtered before anything was typed.
    await page.goto(`/nodes?kind=device&sel=node:${TREE_SIBLING_IDS[0]}&tab=events`);
    await expect(page.getByRole('tab', { selected: true })).toHaveText(/^Events/);
    await expect(pane(page).getByRole('group', { name: 'Column filters' })).toBeVisible();
    await expect(pane(page).getByRole('button', { name: /Clear all filters/ }), 'the tab read the tree’s key').toHaveCount(0);

    await typeFilter(page, pane(page), 'Message', NEEDLE);
    // 🚨 The Escape that closed the filter popover is not the page's. Since the filter writes the URL,
    // the page's own Escape listener is re-registered behind the popover's, and without the popover
    // marking the press as used the second listener cleared the selection this tab belongs to.
    await expect(page, 'closing the filter popover with Escape also cleared the selection').toHaveURL(/[?&]sel=node/);
    await expect(page).toHaveURL(new RegExp(`[?&]events\\.message=[^&]*${NEEDLE}`));
    expect(new URL(page.url()).searchParams.get('kind'), 'the tab’s filter overwrote the tree’s').toBe('device');

    await reload(page);
    await expect(pane(page).getByRole('button', { name: /Clear all filters/ }), 'the reload lost the filter').toHaveCount(1);
    await pane(page).getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page).not.toHaveURL(/events\.message=/);
    expect(new URL(page.url()).searchParams.get('kind'), 'the tab’s clear-all took the tree’s filter').toBe('device');
  });

  test('keeps its filter on the next node and after a reload', async ({ page }) => {
    await page.goto(`/nodes?sel=node:${TREE_SIBLING_IDS[0]}`);
    // Pressed rather than arrived at by URL: the tab carries to the next node only once it has been
    // clicked (ADR-134 決定 2), and the next node is where this test looks.
    await page.getByRole('tab', { name: /^Interfaces/ }).click();
    await expect(page.getByRole('tab', { selected: true })).toHaveText(/^Interfaces/);

    await typeFilter(page, pane(page), 'Interface', NEEDLE);
    await expect(page).toHaveURL(new RegExp(`[?&]interfaces\\.if_name=[^&]*${NEEDLE}`));

    // Walk to another switch in the tree: the filter is still in force there.
    await page.locator('.ntree-node').nth(2).click();
    await expect(page).toHaveURL(new RegExp(`sel=node%3A${TREE_SIBLING_IDS[2]}`));
    await expect(page.getByRole('tab', { selected: true })).toHaveText(/^Interfaces/);
    await expect(pane(page).getByRole('button', { name: /Clear all filters/ }), 'the next node lost the filter').toHaveCount(1);

    await reload(page);
    await expect(page.getByRole('tab', { selected: true })).toHaveText(/^Interfaces/);
    await expect(pane(page).getByRole('button', { name: /Clear all filters/ }), 'the reload lost the filter').toHaveCount(1);

    await pane(page).getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page).not.toHaveURL(/interfaces\.if_name=/);
  });
});

test.describe('a one-table screen that kept its filter in component state', () => {
  test('Settings ▸ Users keeps a narrowed list after a reload', async ({ page }) => {
    await page.goto('/settings/users');
    await typeFilter(page, page, 'Username', NEEDLE);
    await expect(page).toHaveURL(new RegExp(`[?&]q=[^&]*${NEEDLE}`));
    await reload(page);
    await expect(page.getByRole('button', { name: /Clear all filters/ }), 'the reload lost the filter').toHaveCount(1);
  });

  test('Saved findings opens on the scope its URL names, not on All nodes', async ({ page }) => {
    // The scope is not a column: its ids ride beside the columns, and the picker's label is derived
    // from them on arrival. A picker reading "All nodes" over a list narrowed to one node is the
    // untrue half of the pair.
    await page.goto(`/troubleshoot/findings?node_id=${TREE_SIBLING_IDS[1]}`);
    const picker = page.locator('.scope-picker-label');
    await expect(picker).toHaveText(/^node: /);
    await expect(page.getByRole('button', { name: /Clear all filters/ })).toHaveCount(1);

    await page.getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page).not.toHaveURL(/node_id=/);
    await expect(picker).toHaveText('All nodes');
  });
});

test.describe('a chip, a sort select and a sortable header', () => {
  test('a Troubleshoot report keeps its chip and its sort after a reload, and keeps its job', async ({ page }) => {
    const job = '00000000-0000-4000-8000-000000000002';
    await page.goto(`/troubleshoot/report/${REPORT_TOOL}?job=${job}`);
    const chip = page.getByRole('button', { name: 'Level shift' });
    await chip.click();
    await expect(page).toHaveURL(/[?&]filter=level(&|$)/);
    await page.locator('#tsr-anomaly-sort').selectOption('node');
    await expect(page).toHaveURL(/[?&]sort=node(&|$)/);

    await reload(page);
    await expect(chip, 'the reload dropped the chip').toHaveAttribute('aria-pressed', 'true');
    await expect(page.locator('#tsr-anomaly-sort'), 'the reload dropped the sort').toHaveValue('node');
    expect(new URL(page.url()).searchParams.get('job'), 'the chip write took the job with it').toBe(job);
  });

  test('API tokens keeps its sort after a reload', async ({ page }) => {
    await page.goto('/settings/api-tokens');
    const header = page.getByRole('button', { name: /^Name/ });
    await header.click();
    await expect(header).toHaveAttribute('aria-sort', 'ascending');
    await expect(page).toHaveURL(/[?&]sort=name&dir=asc/);

    await reload(page);
    await expect(page.getByRole('button', { name: /^Name/ }), 'the reload dropped the sort').toHaveAttribute(
      'aria-sort',
      'ascending',
    );
  });
});

test.describe('Notification delivery — two tables with the same column keys', () => {
  const section = (page: Page, title: string) =>
    page.locator('section').filter({ has: page.getByRole('heading', { name: title }) });

  test('a channels filter survives a reload and does not narrow the rules table', async ({ page }) => {
    await page.goto('/alerts/routing');
    const channels = section(page, 'Notification channels');
    const rules = section(page, 'Routing rules');
    await expect(channels).toHaveCount(1);
    await expect(rules).toHaveCount(1);

    await typeFilter(page, channels, 'Name', NEEDLE);
    await expect(page).toHaveURL(new RegExp(`[?&]channels\\.name=[^&]*${NEEDLE}`));
    await expect(channels.getByRole('button', { name: /Clear all filters/ })).toHaveCount(1);
    await expect(rules.getByRole('button', { name: /Clear all filters/ }), 'the rules table picked up the channels filter').toHaveCount(0);

    await reload(page);
    await expect(channels.getByRole('button', { name: /Clear all filters/ }), 'the reload dropped the filter').toHaveCount(1);
    await expect(rules.getByRole('button', { name: /Clear all filters/ })).toHaveCount(0);

    await channels.getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page).not.toHaveURL(/channels\.name=/);
  });
});
