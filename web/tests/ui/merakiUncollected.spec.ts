// SPDX-License-Identifier: AGPL-3.0-only
// A monitored Meraki device in a network the organization does not watch is said to be quiet, on
// both screens that show the organization (ADR-164 決定 15).
//
// WHY A BROWSER. `uncollectedDevices` is unit-tested, but which sentence it becomes, whether the
// button is there, and **which networks the button sends** are wiring in `MerakiOrgPage.tsx` and
// `MerakiSyncStatus.tsx`, which Vitest never runs (testing.md). The last one is the one that
// matters: sending every unwatched network would bring the quiet nodes back *and* start importing
// from networks nobody asked about, and the page would look exactly the same.
//
// The bodies are the generated ones, patched — a hand-written fixture would be a second copy of the
// contract (testing.md). The one write pressed here is answered by the mock.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { MERAKI_ORG_SCREEN } from './screens';

type Schemas = components['schemas'];

const QUIET_NETWORK = 'ymock-net-quiet';
const OTHER_UNWATCHED = 'ymock-net-unimported';

/** One sync behind it (so the counts are drawn), and one node nothing is collected for. */
const orgs = (() => {
  const body = defaultBodyFor('/api/v1/meraki/orgs') as unknown as Schemas['MerakiOrgView'][];
  body[0] = {
    ...body[0],
    name: 'ymock-acme',
    // Off on purpose: the older "N networks are not watched" notice is about automatic import and
    // stays away, so the one notice on the page is the one under test.
    import_devices: false,
    devices_over_cap: 0,
    last_sync_at: '2026-09-20T00:00:00Z',
    last_sync_ok: true,
    last_sync_error: null,
    devices: { seen: 3, monitored: 2, new: 1, missing: 0, monitored_unwatched: 1 },
  };
  return body as unknown as Json;
})();

/** A quiet node, a healthy node, and a device that is not a node in *another* unwatched network. */
const devices = (() => {
  const [first] = defaultBodyFor(
    '/api/v1/meraki/orgs/{id}/devices',
  ) as unknown as Schemas['MerakiDeviceView'][];
  const node = (serial: string, network_id: string, watched: boolean): Schemas['MerakiDeviceView'] => ({
    ...first,
    serial,
    name: serial,
    state: 'monitored',
    network_id,
    network_name: network_id,
    network_monitored: watched,
    filing: null,
  });
  const fresh: Schemas['MerakiDeviceView'] = {
    ...first,
    serial: 'ymock-serial-new',
    name: 'ymock-serial-new',
    state: 'new',
    node_id: null,
    folder_id: null,
    network_id: OTHER_UNWATCHED,
    network_name: OTHER_UNWATCHED,
    network_monitored: false,
    filing: { reason: 'unmatched', prefix: null, folders: null },
  };
  return [
    node('ymock-serial-quiet', QUIET_NETWORK, false),
    node('ymock-serial-fine', 'ymock-net-watched', true),
    fresh,
  ] as unknown as Json;
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

test('the organization’s row says how many of its nodes are not collected', async ({ page }) => {
  await page.goto('/settings/integrations/meraki');
  const status = page.locator('.meraki-org-sync');
  await expect(status).toContainText('2 of 3 devices monitored');
  await expect(status).toContainText('1 not collected');
});

test('the page says why, and the button watches only the network the quiet node is in', async ({
  page,
  errors,
}) => {
  await page.goto(MERAKI_ORG_SCREEN);
  const notice = page.locator('.meraki-orgpage-uncollected');
  await expect(notice).toContainText(
    '1 monitored device is in a network that is not watched, so nothing is collected for it.',
  );
  // The only notice: automatic import is off and nothing is over the cap.
  await expect(page.locator('.meraki-orgpage-notice')).toHaveCount(1);

  const asked = page.waitForRequest(
    (r) => r.method() === 'PUT' && new URL(r.url()).pathname.endsWith('/networks'),
  );
  await notice.getByRole('button', { name: 'Watch these networks' }).click();
  const body = (await asked).postDataJSON() as { network_ids: string[]; monitored: boolean };
  expect(body).toEqual({ network_ids: [QUIET_NETWORK], monitored: true });
  // Said outright, because it is the point: the other unwatched network holds no node.
  expect(body.network_ids).not.toContain(OTHER_UNWATCHED);
  expect(errors.uncaught).toEqual([]);
});
