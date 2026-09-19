// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  MERAKI_PAGE_PATH,
  canSyncNow,
  merakiOrgPath,
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
