// SPDX-License-Identifier: AGPL-3.0-only
// A folder's pane offers Maintenance… the way a node's header does (ADR-209 decision 11): the same
// dialog, its scope fixed to the folder, and the window it saves is written on the folder rather
// than on its members one by one.
//
// Why Tier1: which button the pane draws and what the dialog it opens is told are wiring between
// `NodesPage` and `GroupDetail`, both `.tsx`, which Vitest never loads.
//
// The second half is the defect this button's first run found: the dialog reads the folder's time
// zone, and a name the browser cannot compute made `Intl` throw and blanked the whole WebUI. The
// judgement is `maintenanceTime.ts::targetZone` (unit-tested); this proves the dialog goes through it.

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { type Json } from '../support/openapi';
import { CHILD_ID, CHILD_NAME, membersByGroup, twoLevelGroups } from '../support/twoLevelTree';

/** The two-level tree with the inner folder keeping time in `zone`. */
function groupsWithZone(zone: string): Json {
  return (twoLevelGroups() as Record<string, Json>[]).map((g) =>
    g.id === CHILD_ID ? { ...g, time_zone: zone, effective_time_zone: zone } : g,
  ) as unknown as Json;
}

/** Open the inner folder's pane and press its Maintenance… button; answers the dialog. */
async function openFolderMaintenance(page: Page) {
  await page.goto(`/nodes?sel=group:${CHILD_ID}`);
  await expect(page.locator('.nd-grpbody')).toBeVisible();
  await page
    .locator('.nd-actions')
    .getByRole('button', { name: 'Maintenance…', exact: true })
    .click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByText('Add maintenance window')).toBeVisible();
  return dialog;
}

test.describe('a folder with a time zone', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': groupsWithZone('Asia/Tokyo'),
        '/api/v1/nodes/by-group': membersByGroup,
      },
    },
  });

  test("its Maintenance… sits beside Edit and saves a window on the folder", async ({
    page,
    errors,
  }) => {
    await page.goto(`/nodes?sel=group:${CHILD_ID}`);
    // Beside Edit group, as on a node's header. Both wait for the permissions to resolve (`useCan`
    // answers false until then), and `allTextContents` does not wait at all.
    const actions = page.locator('.nd-actions');
    await expect(actions.getByRole('button', { name: 'Edit group', exact: true })).toBeVisible();
    await expect(actions.getByRole('button', { name: 'Maintenance…', exact: true })).toBeVisible();
    const labels = await actions.getByRole('button').allTextContents();
    const edit = labels.indexOf('Edit group');
    expect(edit).toBeGreaterThanOrEqual(0);
    expect(labels[edit + 1]).toBe('Maintenance…');

    const dialog = await openFolderMaintenance(page);
    // The scope is the folder, named, and not a control the operator could change.
    await expect(dialog.getByText('Folder group', { exact: false })).toBeVisible();
    await expect(dialog.locator('strong', { hasText: CHILD_NAME })).toBeVisible();
    await expect(dialog.getByText('(incl. subgroups)', { exact: false })).toBeVisible();

    const post = page.waitForRequest(
      (r) => r.method() === 'POST' && new URL(r.url()).pathname === '/api/v1/maintenance-windows',
    );
    await dialog.getByRole('button', { name: 'Add window' }).click();
    const body = (await post).postDataJSON() as Record<string, string>;
    expect(body).toMatchObject({
      scope_level: 'group_id',
      scope_id: CHILD_ID,
      name: `Maintenance — ${CHILD_NAME}`,
    });
    expect(errors.uncaught).toEqual([]);
  });
});

test.describe('a folder whose zone this browser cannot compute', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': groupsWithZone('Not/A_Zone'),
        '/api/v1/nodes/by-group': membersByGroup,
      },
    },
  });

  test('its Maintenance… still opens the dialog, on the display zone', async ({ page, errors }) => {
    const dialog = await openFolderMaintenance(page);
    await expect(dialog.locator('strong', { hasText: CHILD_NAME })).toBeVisible();
    await expect(dialog.getByRole('button', { name: 'Add window' })).toBeEnabled();
    expect(errors.uncaught).toEqual([]);
  });
});
