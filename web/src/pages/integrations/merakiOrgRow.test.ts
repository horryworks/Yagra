// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  MERAKI_PAGE_PATH,
  canSyncNow,
  merakiOrgPath,
  orgCollectFailures,
  orgFullRead,
  orgHasInventory,
  orgSyncSummary,
} from './merakiOrgRow';

describe('orgSyncSummary', () => {
  it('reads an organization that has never synced as never — not as failed', () => {
    // The column is null until a sync has run. `!last_sync_ok` would call this a failure.
    expect(orgSyncSummary({ last_sync_at: null, last_sync_ok: null, last_sync_error: null })).toEqual({
      kind: 'never',
    });
    expect(orgSyncSummary({})).toEqual({ kind: 'never' });
  });

  it('reports a success by its time', () => {
    expect(
      orgSyncSummary({ last_sync_at: '2026-09-19T09:00:00Z', last_sync_ok: true, last_sync_error: null }),
    ).toEqual({ kind: 'ok', at: '2026-09-19T09:00:00Z' });
  });

  it('lets a failure outrank the stamp of an older success', () => {
    // A failed sync leaves `last_sync_at` where the last success put it. Rendering that stamp as
    // "Last sync 09:00" would hide a sync that has been failing ever since.
    expect(
      orgSyncSummary({
        last_sync_at: '2026-09-19T09:00:00Z',
        last_sync_ok: false,
        last_sync_error: 'auth',
      }),
    ).toEqual({ kind: 'failed', reason: 'auth', lastGoodAt: '2026-09-19T09:00:00Z' });
  });

  it('still says failed when the first sync ever is the one that failed', () => {
    expect(orgSyncSummary({ last_sync_at: null, last_sync_ok: false, last_sync_error: 'unreachable' })).toEqual({
      kind: 'failed',
      reason: 'unreachable',
      lastGoodAt: null,
    });
  });

  it('names a reason even when the server sent none', () => {
    expect(orgSyncSummary({ last_sync_ok: false, last_sync_error: null })).toMatchObject({
      kind: 'failed',
      reason: 'internal',
    });
  });
});

describe('orgFullRead (ADR-164 決定 32)', () => {
  const synced = {
    last_sync_at: '2026-09-24T09:00:00Z',
    last_sync_ok: true,
    enabled: true,
    full_sync: null,
  } as const;
  const asked = '2026-09-24T09:05:00Z';

  it('shows a read that has begun by how far it has got', () => {
    expect(
      orgFullRead(
        {
          ...synced,
          full_sync: {
            requested_at: asked,
            started_at: '2026-09-24T09:06:00Z',
            networks: 350,
            read: 120,
          },
        },
        true,
      ),
    ).toEqual({ kind: 'reading', read: 120, networks: 350 });
    // A first read nobody asked for runs too, with no request behind it.
    expect(
      orgFullRead(
        {
          ...synced,
          last_sync_at: null,
          last_sync_ok: null,
          full_sync: {
            requested_at: null,
            started_at: '2026-09-24T09:06:00Z',
            networks: null,
            read: null,
          },
        },
        true,
      ),
    ).toEqual({ kind: 'reading', read: 0, networks: 0 });
  });

  it('shows a request that has not begun as queued — paused or not, since it stands', () => {
    const queued = {
      ...synced,
      full_sync: { requested_at: asked, started_at: null, networks: null, read: null },
    };
    expect(orgFullRead(queued, true)).toEqual({ kind: 'queued' });
    expect(orgFullRead({ ...queued, enabled: false }, false)).toEqual({ kind: 'queued' });
  });

  it('waits for the first read only while something is going to make it', () => {
    const fresh = { ...synced, last_sync_at: null, last_sync_ok: null };
    expect(orgFullRead(fresh, true)).toEqual({ kind: 'first' });
    expect(orgFullRead({ ...fresh, enabled: false }, true)).toEqual({ kind: 'none' });
    expect(orgFullRead(fresh, false)).toEqual({ kind: 'none' });
    // A first sync that failed is a failure on the row, not a read about to happen.
    expect(orgFullRead({ ...fresh, last_sync_ok: false }, true)).toEqual({ kind: 'none' });
  });

  it('says nothing for an organization with no read asked for or running', () => {
    expect(orgFullRead(synced, true)).toEqual({ kind: 'none' });
    expect(orgFullRead({ ...synced, full_sync: undefined }, true)).toEqual({ kind: 'none' });
  });
});

describe('orgHasInventory', () => {
  it('holds the counts back until one sync has succeeded, and keeps them through a failure', () => {
    expect(orgHasInventory({ last_sync_at: null })).toBe(false);
    expect(orgHasInventory({})).toBe(false);
    expect(orgHasInventory({ last_sync_at: '2026-09-19T09:00:00Z' })).toBe(true);
  });
});

describe('canSyncNow', () => {
  it('is offered only when neither switch would refuse it', () => {
    expect(canSyncNow({ enabled: true }, true)).toBe(true);
    expect(canSyncNow({ enabled: false }, true)).toBe(false);
    expect(canSyncNow({ enabled: true }, false)).toBe(false);
    expect(canSyncNow({ enabled: false }, false)).toBe(false);
  });
});

describe('merakiOrgPath', () => {
  it('puts the organization one segment under the Meraki page', () => {
    expect(merakiOrgPath('0b0e6a9e-1c1c-4d5e-8a3f-2f4f6a7b8c9d')).toBe(
      '/settings/integrations/meraki/0b0e6a9e-1c1c-4d5e-8a3f-2f4f6a7b8c9d',
    );
  });

  it('is a path the settings routes actually serve', () => {
    // The link and the route are spelled in two files. A route renamed without this helper (or the
    // other way round) compiles, and the row's name then lands on the settings group's catch-all —
    // which redirects to the dashboard, so the link "works" and goes nowhere near the organization.
    const routes = readFileSync(join(__dirname, '../../routeGroups/settings.tsx'), 'utf8');
    const under = MERAKI_PAGE_PATH.replace('/settings/', '');
    expect(routes).toContain(`path="${under}"`);
    expect(routes).toContain(`path="${under}/:orgId"`);
  });
});

describe('which of an organization’s collects are failing (ADR-164 決定 18)', () => {
  const failing = (tier: string, reason: string) => ({
    tier,
    reason: reason as never,
    since: '2026-09-20T00:00:00Z',
    failures: 3,
  });

  it('says nothing for an organization that is being answered', () => {
    expect(orgCollectFailures({ collect_failures: [] })).toEqual([]);
  });

  it('puts the tier a device’s state rides on first, whatever order the server sent', () => {
    const got = orgCollectFailures({
      collect_failures: [failing('traffic', 'upstream'), failing('availability', 'auth')],
    });
    expect(got).toEqual([
      { tier: 'availability', reason: 'auth', stalesNodes: true, listing: null },
      { tier: 'traffic', reason: 'upstream', stalesNodes: false, listing: null },
    ]);
  });

  it('only availability leaves the nodes at their last state', () => {
    const got = orgCollectFailures({
      collect_failures: [failing('uplink', 'auth'), failing('traffic', 'auth')],
    });
    expect(got.map((f) => f.stalesNodes)).toEqual([false, false]);
  });

  it('reads a reason this bundle has never heard of as a failure, never as a raw key', () => {
    const got = orgCollectFailures({
      collect_failures: [failing('availability', 'a_reason_from_the_future')],
    });
    expect(got).toEqual([{ tier: 'availability', reason: 'internal', stalesNodes: true, listing: null }]);
  });

  it('names which read of the tier failed, and reads one this bundle does not know as none (ADR-164 決定 25)', () => {
    const got = orgCollectFailures({
      collect_failures: [
        { ...failing('uplink', 'upstream'), listing: 'appliance_vpn_statuses' },
        { ...failing('traffic', 'upstream'), listing: 'a_listing_from_the_future' },
      ],
    });
    expect(got.map((f) => f.listing)).toEqual(['appliance_vpn_statuses', null]);
  });

  it('carries `no_answer` through: a collect that was sent and never came back', () => {
    const got = orgCollectFailures({ collect_failures: [failing('availability', 'no_answer')] });
    expect(got[0].reason).toBe('no_answer');
  });
});
