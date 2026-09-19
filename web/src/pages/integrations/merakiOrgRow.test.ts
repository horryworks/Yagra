// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { canSyncNow, orgHasInventory, orgSyncSummary } from './merakiOrgRow';

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
