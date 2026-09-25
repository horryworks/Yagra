// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { anySyncInProgress, syncProgress } from './netboxStatus';

// ADR-172 決定 1: "Sync now" answers 202 and the run happens in the leader's loop, so what the row
// shows while it goes is read from `sync`, never from the answer to the button.

describe('syncProgress', () => {
  it('is none when nothing is asked for or running', () => {
    expect(syncProgress({})).toEqual({ kind: 'none' });
    expect(syncProgress({ sync: null })).toEqual({ kind: 'none' });
    expect(syncProgress({ sync: { requested_at: null, started_at: null } })).toEqual({
      kind: 'none',
    });
  });

  it('is queued while the request waits for the loop', () => {
    expect(syncProgress({ sync: { requested_at: '2026-09-25T00:00:00Z', started_at: null } })).toEqual(
      { kind: 'queued' },
    );
  });

  it('reads a run in flight as running even when a request is also waiting', () => {
    // A press during a scheduled run survives that run, so both are set at once. NetBox is
    // already being read — "waiting" would be the wrong thing to say.
    expect(
      syncProgress({
        sync: { requested_at: '2026-09-25T00:00:05Z', started_at: '2026-09-25T00:00:00Z' },
      }),
    ).toEqual({ kind: 'running' });
    expect(syncProgress({ sync: { started_at: '2026-09-25T00:00:00Z' } })).toEqual({
      kind: 'running',
    });
  });
});

describe('anySyncInProgress', () => {
  it('keeps the page re-reading while any row will change by itself', () => {
    expect(anySyncInProgress([])).toBe(false);
    expect(anySyncInProgress([{ sync: null }, {}])).toBe(false);
    expect(
      anySyncInProgress([{ sync: null }, { sync: { requested_at: '2026-09-25T00:00:00Z' } }]),
    ).toBe(true);
  });
});
