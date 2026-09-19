// SPDX-License-Identifier: AGPL-3.0-only
// One Meraki organization's page shows the settings and the devices it was served (ADR-164 Inc.4/5).
//
// WHY THIS IS NOT LEFT TO THE ROUTE WALK. The walk does open this screen, but under the generated
// mock it sees exactly one kind of row: every enum answers with its first member, so the one device
// is `monitored` — a node already. Everything the page exists for is on the *other* kind of row:
// the checkbox, where an import would file the device and why, the network that automatic import
// cannot reach. With only the generated body, deleting all of that leaves the walk green.
//
// Vitest cannot be the reader either. `merakiDevices.ts` is unit-tested, but the wiring from its
// answers to a cell lives in `MerakiOrgPage.tsx`, which Vitest never runs (testing.md).
//
// Read-only on purpose: it looks, and presses nothing that writes. The bodies are the generated
// ones, patched — a hand-written fixture would be a second copy of the contract (testing.md).

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { MERAKI_ORG_SCREEN } from './screens';

type Schemas = components['schemas'];

const ORG_NAME = 'ymock-acme';

/** Automatic import on, a cap that is visibly not the generator's `1`, and devices left over it. */
const orgs = (() => {
  const body = defaultBodyFor('/api/v1/meraki/orgs') as unknown as Schemas['MerakiOrgView'][];
  body[0] = {
    ...body[0],
    name: ORG_NAME,
    import_devices: true,
    file_by_prefix: true,
    max_devices: 500,
    devices_over_cap: 3,
  };
  return body as unknown as Json;
})();

/** Two rows, one of each kind: a node, and a device an import would put under the organization. */
const devices = (() => {
  const [first] = defaultBodyFor(
    '/api/v1/meraki/orgs/{id}/devices',
  ) as unknown as Schemas['MerakiDeviceView'][];
  const node: Schemas['MerakiDeviceView'] = {
    ...first,
    serial: 'ymock-serial-node',
    name: 'ymock-node-device',
    state: 'monitored',
    // The generator answers a boolean with `false`, which would mark *both* rows "not watched" and
    // leave the mark unable to tell one row from the other.
    network_monitored: true,
    // A node is not going to be filed anywhere, so the server sends no filing for it.
    filing: null,
  };
  const fresh: Schemas['MerakiDeviceView'] = {
    ...first,
    serial: 'ymock-serial-new',
    name: 'ymock-new-device',
    state: 'new',
    node_id: null,
    folder_id: null,
    network_name: 'ymock-branch',
    network_monitored: false,
    filing: { reason: 'unmatched', prefix: null, folders: null },
  };
  return [node, fresh] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/meraki/orgs': orgs,
      '/api/v1/meraki/orgs/{id}/devices': devices,
    },
  },
});

test('the page says what it is, and carries the organization’s name', async ({ page, errors }) => {
  await page.goto(MERAKI_ORG_SCREEN);
  await expect(page.locator('.pageheader-title')).toHaveText(ORG_NAME);
  // ADR-055 R2: one line under the title, on the screen, not behind a hover.
  const note = page.locator('.pageheader-note');
  await expect(note).toHaveCount(1);
  expect((await note.innerText()).trim().length).toBeGreaterThan(0);
  expect(errors.uncaught).toEqual([]);
});

test('the settings card shows the stored import settings', async ({ page }) => {
  await page.goto(MERAKI_ORG_SCREEN);
  const card = page.locator('.meraki-orgpage-settings');
  await expect(card).toHaveCount(1);

  // Both switches and the cap, as served. `toBeChecked` reads the DOM property, which is the
  // question here — the mock's `true` reached the control.
  const boxes = card.locator('input[type="checkbox"]');
  await expect(boxes).toHaveCount(2);
  await expect(boxes.nth(0)).toBeChecked();
  await expect(boxes.nth(1)).toBeChecked();
  await expect(card.locator('input[type="number"]')).toHaveValue('500');

  // The second switch names the folder everything else goes under: the organization's own.
  await expect(card).toContainText(ORG_NAME);

  // The two notices under it, each through its own sentence. A class alone would pass on either.
  const notices = page.locator('.meraki-orgpage-notice');
  await expect(notices).toHaveCount(2);
  // The generated network list is one network, not watched.
  await expect(notices.nth(0)).toContainText('1 network is not watched');
  await expect(notices.nth(1)).toContainText('3 devices were not imported');
});

test('the device table shows both kinds of row, and says where the new one would go', async ({
  page,
}) => {
  await page.goto(MERAKI_ORG_SCREEN);
  const rows = page.locator('.dt-row');
  await expect(rows).toHaveCount(2);

  const node = rows.filter({ hasText: 'ymock-node-device' });
  const fresh = rows.filter({ hasText: 'ymock-new-device' });

  // A node links to itself and has nothing to tick; a device that is not one is the reverse.
  await expect(node.locator('a[href^="/nodes/"]')).toHaveCount(1);
  await expect(node.locator('input[type="checkbox"]')).toHaveCount(0);
  await expect(fresh.locator('a[href^="/nodes/"]')).toHaveCount(0);
  await expect(fresh.locator('input[type="checkbox"]')).toHaveCount(1);

  // The state is a word, not a colour.
  await expect(node).toContainText('Monitored');
  await expect(fresh).toContainText('New');

  // Where the import would put it — under the organization, by network — and why.
  await expect(fresh).toContainText(`${ORG_NAME} ▸ ymock-branch`);
  await expect(fresh).toContainText('No matching IP range');
  // …and the mark that automatic import does not reach its network.
  await expect(fresh.locator('.meraki-dev-flag')).toHaveText('not watched');
  await expect(node.locator('.meraki-dev-flag')).toHaveCount(0);

  // Nothing is ticked, so there is nothing to import yet: the button follows the selection.
  await expect(page.getByRole('button', { name: /^Import \d+ device/ })).toHaveCount(0);
});

test('an organization the deployment does not have is said not to exist', async ({ page }) => {
  // Any other uuid: the list the mock serves does not contain it. The failure this guards is the
  // page reading "no such row" as "still loading" and sitting on a spinner forever.
  await page.goto('/settings/integrations/meraki/00000000-0000-4000-8000-00000000dead');
  await expect(page.getByText('This organization does not exist.')).toBeVisible();
  await expect(page.locator('.meraki-orgpage-settings')).toHaveCount(0);
  await expect(page.getByRole('link', { name: 'Back to Cisco Meraki' })).toHaveAttribute(
    'href',
    '/settings/integrations/meraki',
  );
});
