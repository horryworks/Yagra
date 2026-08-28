// SPDX-License-Identifier: AGPL-3.0-only
// The selection dialog: press Upgrade, choose what moves, press again (ADR-051 Inc.6).
//
// Why this needs a browser rather than a Vitest case. `upgradeStatus.ts` already decides which rows
// are locked and what the defaults are, and those are unit-tested; what nothing else can see is
// whether that judgement reaches an actual `<input type="checkbox">`. The rows are rendered in a
// `.tsx`, which Vitest never executes (testing.md), so a dialog that computed the right answer and
// drew every box open — or every box closed — would pass the whole suite.
//
// Three claims, and the third is the one worth the file: the dialog opens with the movable rows
// ticked, a row that cannot move has a **closed and disabled** box with its reason beside it rather
// than in a tooltip (ADR-055 R4), and the co-located poller has no box of its own to be wrong
// about.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

/** A deployment with one release to move to, and four things to move it with: core, the poller that
 *  shares its host, a remote site that can replace itself, and one that cannot.
 *
 *  Patched onto the generated body rather than hand-written, so a change to
 *  `UpgradeStatusResponse` reaches this fixture through the contract instead of leaving it
 *  describing a shape the API no longer returns (ADR-052 決定 2). */
function upgradeStatus(): Json {
  const body = defaultBodyFor('/api/v1/system/upgrade') as {
    enabled: boolean;
    upgrade_enabled: boolean;
    updater: Record<string, unknown>;
    current: { core_version: string };
    offers: unknown[];
    components: unknown[];
    poller_convergence: unknown;
  };
  body.enabled = true;
  body.upgrade_enabled = true;
  body.updater = {
    ...body.updater,
    installed: true,
    present: true,
    fresh: true,
    paused: false,
    allow_bundle: false,
    repo: 'ghcr.io/horryworks',
    check_interval_secs: 86_400,
    last_seen: Math.floor(Date.now() / 1000),
  };
  body.current.core_version = '9.9.9';
  body.offers = [{ tag: 'v9.9.10', core_digest: null, direction: 'upgrade', blocked: null }];
  const row = (over: Record<string, unknown>) => ({
    id: 'x',
    kind: 'poller',
    pool: 'default',
    version: '9.9.9',
    upgradable: true,
    reason: null,
    co_located: false,
    moves_back: false,
    live_in_pool: 3,
    progress: null,
    ...over,
  });
  body.components = [
    row({ id: 'core', kind: 'core', pool: null, upgradable: true, live_in_pool: 0 }),
    row({ id: 'local', co_located: true, reason: 'co_located' }),
    row({ id: 'site-remote' }),
    row({ id: 'site-bare', upgradable: false, reason: 'no_site_updater' }),
  ];
  body.poller_convergence = null;
  return body as unknown as Json;
}

test.use({
  viewport: { width: 1440, height: 900 },
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/system/upgrade': () => upgradeStatus() },
  },
});

/** One row of the dialog, by the component it names. */
function pickRow(page: import('@playwright/test').Page, id: string) {
  return page.locator('.upgrade-pick-row', { hasText: id });
}

test('Upgrade opens the components list rather than starting anything', async ({ page }) => {
  await page.goto('/settings/upgrade');
  await page.getByRole('button', { name: 'Upgrade' }).first().click();

  const dialog = page.locator('[role="dialog"]');
  await expect(dialog).toContainText('Upgrade to v9.9.10');
  // Every component is listed, including the two that cannot be chosen: a row that is dropped reads
  // as "that poller is gone", which is the failure this list exists to stop.
  await expect(dialog.locator('.upgrade-pick-row')).toHaveCount(4);
  for (const id of ['core + web', 'local', 'site-remote', 'site-bare']) {
    await expect(pickRow(page, id)).toHaveCount(1);
  }
  // Nothing has been asked for yet — the dialog *is* the confirmation.
  await expect(dialog.getByRole('button', { name: /Upgrade \d/ })).toBeEnabled();
});

test('the boxes open ticked, and the ones that cannot move are closed and say why', async ({
  page,
}) => {
  await page.goto('/settings/upgrade');
  await page.getByRole('button', { name: 'Upgrade' }).first().click();

  // 🚨 The accepting case first. A dialog that disabled everything would satisfy both exclusions
  // below while making the button useless, and would look exactly like a deployment with nothing
  // to do.
  const core = pickRow(page, 'core + web').locator('input[type="checkbox"]');
  const remote = pickRow(page, 'site-remote').locator('input[type="checkbox"]');
  await expect(core).toBeChecked();
  await expect(core).toBeEnabled();
  await expect(remote).toBeChecked();
  await expect(remote).toBeEnabled();

  // A site with no updater cannot come, and the reason is **in the row**: a control whose
  // explanation is hover-only is one a touch device never explains (ADR-055 R4).
  const bare = pickRow(page, 'site-bare');
  await expect(bare.locator('input[type="checkbox"]')).not.toBeChecked();
  await expect(bare.locator('input[type="checkbox"]')).toBeDisabled();
  await expect(bare).toContainText('no site updater');

  // The co-located poller follows core rather than having a box of its own to disagree with it.
  const local = pickRow(page, 'local');
  await expect(local.locator('input[type="checkbox"]')).toBeDisabled();
  await expect(local.locator('input[type="checkbox"]')).toBeChecked();
  await expect(local).toContainText('replaced with it');

  // The footer counts what is ticked, not what is listed. Two of four here — and it has to move
  // when a box does, or it is a decoration rather than a count.
  const dialog = page.locator('[role="dialog"]');
  await expect(dialog).toContainText('2 of 2 selected');
  await remote.uncheck();
  await expect(dialog).toContainText('1 of 2 selected');
  await expect(dialog.getByRole('button', { name: 'Upgrade 1 component' })).toBeVisible();
});

test('unticking core turns the press into a poller-only request', async ({ page }) => {
  const posted: Array<Record<string, unknown>> = [];
  await page.route('**/api/v1/system/upgrade', async (route) => {
    if (route.request().method() !== 'POST') return route.fallback();
    posted.push(route.request().postDataJSON() as Record<string, unknown>);
    await route.fulfill({
      status: 202,
      contentType: 'application/json',
      body: JSON.stringify({ id: 'run-1', target_tag: 'v9.9.10', maintenance_window_id: null }),
    });
  });
  await page.goto('/settings/upgrade');
  await page.getByRole('button', { name: 'Upgrade' }).first().click();
  await pickRow(page, 'core + web').locator('input[type="checkbox"]').uncheck();
  await page.locator('[role="dialog"]').getByRole('button', { name: /Upgrade \d/ }).click();

  // 🚨 This is the assertion the whole dialog exists for. `include_core` false is what keeps this
  // deployment running, and `pollers` naming exactly one row is what keeps the other one on its
  // build — an omitted list means *every* poller that can move.
  await expect.poll(() => posted.length).toBe(1);
  expect(posted[0]).toEqual({
    target_tag: 'v9.9.10',
    include_core: false,
    pollers: ['site-remote'],
  });
});

// 🚨 The one placement on this page that must not drift.
//
// A poller-only upgrade touches nothing on this host: no image is pulled here, no container of this
// deployment restarts, no maintenance window opens. It therefore needs no central updater — which
// is exactly why `POST /api/v1/system/upgrade/pollers` does not take the `Upgrade` extractor, and
// why `apply_upgrade` reads `st.upgrade` directly instead of demanding it.
//
// Folding the eight cards into five puts that entrance next to the release list, and the release
// list *is* gated on the mechanism. The natural edit — "everything goes inside `state === 'ready'`"
// — removes a working button from precisely the deployments this feature exists for: the ones with
// remote sites and no updater of their own. It is invisible on the test bench, because the bench
// has an updater.
test.describe('with no central updater', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/system/upgrade': () => {
          const body = upgradeStatus() as unknown as {
            enabled: boolean;
            updater: Record<string, unknown>;
            components: Array<Record<string, unknown>>;
          };
          // The mechanism is deployed but has never reported — `mechanism()` calls this `absent`,
          // and the release list is correctly hidden. The components list and the poller-only
          // entrance are not part of that.
          body.enabled = false;
          body.updater = { ...body.updater, present: false, fresh: false };
          // Something to bring across, or the button would be absent for an honest reason.
          body.components[2].version = '9.9.8';
          return body as unknown as Json;
        },
      },
    },
  });

  test('the poller-only entrance is still there', async ({ page }) => {
    await page.goto('/settings/upgrade');
    // The mechanism really is off: the release list is gone, which is what makes this a test about
    // the entrance rather than about a page that renders everything regardless.
    await expect(page.getByText('No updater has reported on this deployment.')).toBeVisible();
    await expect(page.getByRole('button', { name: /Bring them to/ })).toBeVisible();

    await page.getByRole('button', { name: /Bring them to/ }).click();
    const dialog = page.locator('[role="dialog"]');
    await expect(dialog).toContainText('Upgrade to 9.9.9');
    // core is already on it, so the press can only be about the sites.
    await expect(pickRow(page, 'core + web').locator('input[type="checkbox"]')).toBeDisabled();
    await expect(pickRow(page, 'core + web').locator('input[type="checkbox"]')).not.toBeChecked();
    await expect(dialog).toContainText('1 of 1 selected');
  });
});
