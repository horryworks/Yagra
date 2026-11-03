// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { FleetWatch } from '../types/api';
import { isSilenced, nothingPolled, watchChips } from './watchCounts';

const watch = (over: Partial<FleetWatch> = {}): FleetWatch => ({
  no_live_poller: 0,
  no_recent_data: 0,
  pool_polled: 10,
  pollers_known: true,
  ...over,
});

describe('isSilenced', () => {
  it('counts a row held under a root cause, and nothing else', () => {
    expect(isSilenced({ root_cause: 'n-1' })).toBe(true);
    expect(isSilenced({ root_cause: null })).toBe(false);
    expect(isSilenced({})).toBe(false);
  });
});

describe('watchChips', () => {
  it('draws nothing when everything is zero', () => {
    expect(watchChips({ silenced: 0, maintenance: 0, watch: watch() })).toEqual([]);
    expect(watchChips({ silenced: 0, maintenance: 0, watch: null })).toEqual([]);
  });

  it('keeps the fixed order and leaves out the zeros', () => {
    const got = watchChips({ silenced: 5, maintenance: 0, watch: watch({ no_recent_data: 3 }) });
    expect(got.map((c) => c.key)).toEqual(['silenced', 'notWatched']);
    expect(got[1]).toEqual({ key: 'notWatched', count: 3, to: '/nodes?state=unknown' });
  });

  it('adds both reasons into one number and points at the pollers when a pool is the cause', () => {
    const got = watchChips({
      silenced: 0,
      maintenance: 2,
      watch: watch({ no_live_poller: 40, no_recent_data: 1 }),
    });
    expect(got).toEqual([
      { key: 'maintenance', count: 2, to: '/alerts/maintenance' },
      { key: 'notWatched', count: 41, to: '/settings/pollers' },
    ]);
  });
});

describe('nothingPolled', () => {
  it('is true only when every pool-polled node has no live poller', () => {
    expect(nothingPolled(watch({ no_live_poller: 10 }))).toBe(true);
    expect(nothingPolled(watch({ no_live_poller: 9 }))).toBe(false);
    expect(nothingPolled(null)).toBe(false);
  });

  it('is false with no pool-polled node at all (a Meraki-only deployment)', () => {
    expect(nothingPolled(watch({ pool_polled: 0 }))).toBe(false);
  });

  it('is false where poller liveness is unknown (an HA standby)', () => {
    expect(nothingPolled(watch({ no_live_poller: 10, pollers_known: false }))).toBe(false);
  });
});
