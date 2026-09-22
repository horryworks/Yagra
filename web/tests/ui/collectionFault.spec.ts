// SPDX-License-Identifier: AGPL-3.0-only
// When the Cisco Meraki Dashboard API stops answering an organization, there is ONE alert — about
// the organization — and its nodes keep the last state they had (ADR-164 決定 18). Three screens have
// to say that the same way, and none of them can be reached from Vitest.
//
// WHY A BROWSER. `collectionFaultNotice`, `orgCollectFailures` and `alertSubject` are unit-tested.
// What is not: that the node's own page *renders* the line (a node sitting at `ok` with no alert of
// its own is the whole hazard — if the line is missing the page looks perfectly healthy), that the
// alert row names the organization instead of handing `meraki_org:<id>` to the node-name resolver,
// and that the organization's row words the two kinds of failing collect differently. All three
// are wiring in `.tsx` files, which Vitest never runs (testing.md).
//
// The bodies are the generated ones, patched — a hand-written fixture would be a second copy of the
// contract (testing.md). `BOOTSTRAP_OVERRIDES` clears both fields for every other spec, so this is
// the one place they are set.

import type { components } from '../../src/api/schema';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';

type Schemas = components['schemas'];

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';
const ORG_ID = '00000000-0000-4000-8000-000000000ace';
const ORG_NAME = 'ymock-acme';
const ORG_PATH = `/settings/integrations/meraki/${ORG_ID}`;

/** A node whose state is the last one collected: no alert of its own, and the fault. */
const staleStatus = (() => {
  const body = defaultBodyFor('/api/v1/nodes/{node_id}/status') as unknown as Schemas['NodeStatus'];
  return {
    ...body,
    node_id: NODE_ID,
    state: 'ok',
    alerts: [],
    collection_fault: {
      cause: 'meraki_api',
      meraki_org: ORG_ID,
      meraki_org_name: ORG_NAME,
      reason: 'auth',
      since_unix_ms: 1_790_000_000_000,
    },
  } as unknown as Json;
})();

/** The organization, with the tier liveness rides on failing — and one that only costs readings. */
const orgs = (() => {
  const body = defaultBodyFor('/api/v1/meraki/orgs') as unknown as Schemas['MerakiOrgView'][];
  body[0] = {
    ...body[0],
    id: ORG_ID,
    name: ORG_NAME,
    collect_failures: [
      // Sent in this order on purpose: the row must put availability first whatever arrives.
      { tier: 'traffic', reason: 'upstream', since: '2026-09-20T00:00:00Z', failures: 2 },
      // One read of the uplink tier failed while the others answered (ADR-164 決定 25).
      {
        tier: 'uplink',
        reason: 'upstream',
        since: '2026-09-20T00:00:00Z',
        failures: 1,
        listing: 'appliance_vpn_statuses',
      },
      { tier: 'availability', reason: 'no_answer', since: '2026-09-20T00:00:00Z', failures: 4 },
    ],
  };
  return body as unknown as Json;
})();

/** The one alert: about the organization, never about a node. */
const alerts = (() => {
  const [first] = defaultBodyFor('/api/v1/alerts') as unknown as Schemas['ActiveAlertView'][];
  return [
    {
      ...first,
      node: `meraki_org:${ORG_ID}`,
      subject_kind: 'meraki_org',
      subject_name: ORG_NAME,
      severity: 'critical',
      state: 'critical',
      metric: 'meraki_api_collect',
      root_cause: null,
      acked: null,
    },
  ] as unknown as Json;
})();

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID),
      '/api/v1/nodes/{node_id}/status': staleStatus,
      '/api/v1/meraki/orgs': orgs,
      '/api/v1/alerts': alerts,
    },
  },
});

test('a node whose organization is not being answered says its state is the last one collected', async ({
  page,
  errors,
}) => {
  await page.goto(`/nodes/${NODE_ID}?tab=overview`);
  const notice = page.locator('.nd-fault');
  await expect(notice).toBeVisible({ timeout: 15_000 });
  await expect(notice).toContainText(
    `The Meraki API is not answering for ${ORG_NAME} (Meraki refused the API key).`,
  );
  await expect(notice).toContainText('The state shown here is the last one collected');
  // It points at the page that says which collect is failing — and offers the fix.
  await expect(notice.getByRole('link', { name: 'Open the organization' })).toHaveAttribute(
    'href',
    ORG_PATH,
  );
  expect(errors.uncaught).toEqual([]);
});

test('the organization’s row says which collect is failing, the one that stales its nodes first', async ({
  page,
}) => {
  await page.goto('/settings/integrations/meraki');
  const status = page.locator('.meraki-org-sync');
  await expect(status).toContainText(
    'Collection failing: the poller did not answer. Device states are the last ones collected.',
  );
  await expect(status).toContainText('Traffic collection failing: the Meraki API answered an error');
  // …and a tier whose one read failed says which read.
  await expect(status).toContainText(
    'Uplink (Auto VPN statuses) collection failing: the Meraki API answered an error',
  );
  const lines = await status.locator('.meraki-org-sync-failed').allInnerTexts();
  const stale = lines.findIndex((l) => l.startsWith('Collection failing'));
  const readings = lines.findIndex((l) => l.startsWith('Traffic collection failing'));
  expect(stale).toBeGreaterThanOrEqual(0);
  expect(readings).toBeGreaterThan(stale);
});

// ADR-064 増分 G: an access point no wireless controller has reported lately. The same field, a
// second cause — and a different sentence: the AP reads `unknown`, there is no organization to open,
// and the controller is named by the facts row above the line, so the line itself links nowhere.
test.describe('an access point its controller stopped reporting', () => {
  const AP_ID = '00000000-0000-4000-8000-0000000000ab';
  const apNode = () => {
    const body = deviceNode(AP_ID) as unknown as { kind: string; snmp_configured: boolean };
    body.kind = 'wireless_ap';
    body.snmp_configured = false;
    return body as unknown as Json;
  };
  const apStatus = (() => {
    const body = defaultBodyFor('/api/v1/nodes/{node_id}/status') as unknown as Schemas['NodeStatus'];
    return {
      ...body,
      node_id: AP_ID,
      state: 'unknown',
      alerts: [],
      collection_fault: { cause: 'wireless_controller', since_unix_ms: 1_790_000_000_000 },
    } as unknown as Json;
  })();

  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/{node_id}': apNode,
        '/api/v1/nodes/{node_id}/status': apStatus,
      },
    },
  });

  test('says no controller has reported it since when, and that its state is not known', async ({
    page,
    errors,
  }) => {
    await page.goto(`/nodes/${AP_ID}?tab=overview`);
    const notice = page.locator('.nd-fault');
    await expect(notice).toBeVisible({ timeout: 15_000 });
    await expect(notice).toContainText(
      'No report on this access point from its wireless controller since',
    );
    await expect(notice).toContainText('Until one arrives, its current state is not known.');
    // Nothing about Meraki, and no link of its own.
    await expect(notice).not.toContainText('Meraki');
    await expect(notice.getByRole('link')).toHaveCount(0);
    expect(errors.uncaught).toEqual([]);
  });
});

test('the alert is about the organization, by name, and links to it', async ({ page, errors }) => {
  await page.goto('/alerts');
  const subject = page.getByRole('link', { name: `Meraki organization “${ORG_NAME}”` });
  await expect(subject).toBeVisible({ timeout: 15_000 });
  await expect(subject).toHaveAttribute('href', ORG_PATH);
  // Never rendered as a node: the flat form must not reach the screen as an unresolvable id.
  await expect(page.getByText(`meraki_org:${ORG_ID}`)).toHaveCount(0);
  expect(errors.uncaught).toEqual([]);
});
