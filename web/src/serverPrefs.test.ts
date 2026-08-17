// SPDX-License-Identifier: AGPL-3.0-only
// The account↔browser preference sync's judgement (ADR-058, `serverPrefs.ts`).
//
// ⚠️ **Why this file is worth more than its size suggests.** `serverPrefs.ts` is written to swallow
// every failure — "Nothing here surfaces an error, ever" — so a regression in it produces no toast,
// no console error and no failing screen. The browser-local value keeps working, which is exactly
// the outcome it is designed to produce when the *server* is the thing that is broken. That makes
// silent inertness and correct behaviour indistinguishable from the outside, and leaves this the
// only place either can be told from the other.
//
// The three properties that carry real cost if they break:
//   1. a burst of adjustments coalesces into ONE PUT — every PUT writes an audit row, and the
//      backend has no per-route opt-out, so this is a contract rather than a nicety;
//   2. a failed GET stops later saves for the session — otherwise a deployment on an N-1 core PUTs
//      into a 404 every 800ms for as long as someone is dragging;
//   3. sign-out cancels a pending save — a save queued by the previous account must not land on the
//      next one's row.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const getPreferences = vi.fn();
const putPreferences = vi.fn();
const getToken = vi.fn();

vi.mock('./services/api', () => ({
  api: {
    getPreferences: () => getPreferences(),
    putPreferences: (prefs: unknown) => putPreferences(prefs),
  },
  getToken: () => getToken(),
}));

import { usePrefsStore } from './prefs';
import { loadServerPrefs, resetServerPrefs, setInterfaceDockHeight } from './serverPrefs';

/** The debounce in `serverPrefs.ts`. Restated rather than imported — it is not exported, and a test
 *  that read it from the module could not notice the value changing. */
const SAVE_DEBOUNCE_MS = 800;

beforeEach(() => {
  vi.useFakeTimers();
  getPreferences.mockReset();
  putPreferences.mockReset().mockResolvedValue({ ok: true });
  getToken.mockReset().mockReturnValue('session-token');
  // Module-level `supported` / `saveTimer` survive between tests; this is the documented reset.
  resetServerPrefs();
  usePrefsStore.getState().setInterfaceDockHeight(null);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('loadServerPrefs', () => {
  it('does not call the server when nobody is signed in', async () => {
    getToken.mockReturnValue(null);
    await loadServerPrefs();
    expect(getPreferences).not.toHaveBeenCalled();
  });

  it('adopts the account\'s dock height into the local store', async () => {
    getPreferences.mockResolvedValue({ interfaceDockHeight: 420 });
    await loadServerPrefs();
    expect(usePrefsStore.getState().interfaceDockHeight).toBe(420);
  });

  it('ignores a body that is not an object', async () => {
    // The backend validates only that the document *is* a JSON object, and an older or newer WebUI
    // may have written the row — so a surprising shape is a thing to survive, not to report.
    for (const body of [null, undefined, 'nope', 42, []]) {
      usePrefsStore.getState().setInterfaceDockHeight(null);
      getPreferences.mockResolvedValue(body);
      await loadServerPrefs();
      expect(usePrefsStore.getState().interfaceDockHeight).toBeNull();
    }
  });

  it('ignores a dock height that is not a finite number', async () => {
    for (const height of ['420', null, Number.NaN, Number.POSITIVE_INFINITY, {}]) {
      usePrefsStore.getState().setInterfaceDockHeight(null);
      getPreferences.mockResolvedValue({ interfaceDockHeight: height });
      await loadServerPrefs();
      expect(usePrefsStore.getState().interfaceDockHeight).toBeNull();
    }
  });

  it('never rejects when the endpoint is missing or the network drops', async () => {
    getPreferences.mockRejectedValue(new Error('404 not found'));
    await expect(loadServerPrefs()).resolves.toBeUndefined();
  });

  it('leaves the browser-local value in place when the load fails', async () => {
    usePrefsStore.getState().setInterfaceDockHeight(300);
    getPreferences.mockRejectedValue(new Error('network'));
    await loadServerPrefs();
    expect(usePrefsStore.getState().interfaceDockHeight).toBe(300);
  });
});

describe('setInterfaceDockHeight', () => {
  it('writes the local store immediately, before any request', () => {
    setInterfaceDockHeight(360);
    expect(usePrefsStore.getState().interfaceDockHeight).toBe(360);
    expect(putPreferences).not.toHaveBeenCalled();
  });

  it('saves once after the quiet period', () => {
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledTimes(1);
    expect(putPreferences).toHaveBeenCalledWith({ interfaceDockHeight: 360 });
  });

  it('coalesces a burst into ONE save carrying the last value', () => {
    // The drag case. Every PUT writes an audit row, so the count is the assertion — not just that
    // the final value arrived.
    for (const px of [200, 220, 260, 300, 340]) {
      setInterfaceDockHeight(px);
      vi.advanceTimersByTime(50);
    }
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledTimes(1);
    expect(putPreferences).toHaveBeenCalledWith({ interfaceDockHeight: 340 });
  });

  it('does not save while signed out', () => {
    getToken.mockReturnValue(null);
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).not.toHaveBeenCalled();
    // …but the local value is still recorded: signing out does not undo the adjustment.
    expect(usePrefsStore.getState().interfaceDockHeight).toBe(360);
  });

  it('stops saving for the session once the endpoint has answered badly', async () => {
    // The N-1 core case: without this, a drag PUTs into a 404 every 800ms for the rest of the
    // session. Marking it unsupported on a *transient* failure only costs this session's syncing.
    getPreferences.mockRejectedValue(new Error('404 not found'));
    await loadServerPrefs();

    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).not.toHaveBeenCalled();
  });

  it('resumes saving after a successful load', async () => {
    getPreferences.mockRejectedValueOnce(new Error('transient'));
    await loadServerPrefs();
    getPreferences.mockResolvedValue({});
    await loadServerPrefs();

    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledTimes(1);
  });

  it('never throws when the save itself fails', async () => {
    putPreferences.mockRejectedValue(new Error('500'));
    setInterfaceDockHeight(360);
    expect(() => vi.advanceTimersByTime(SAVE_DEBOUNCE_MS)).not.toThrow();
    await vi.runAllTimersAsync();
    expect(putPreferences).toHaveBeenCalledTimes(1);
  });
});

describe('resetServerPrefs', () => {
  it('cancels a save the previous account queued', () => {
    // Sign-out ordering: the pending PUT would otherwise fire after the next sign-in and write the
    // previous account's dock height onto this one's row.
    setInterfaceDockHeight(360);
    resetServerPrefs();
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS * 2);
    expect(putPreferences).not.toHaveBeenCalled();
  });

  it('re-enables syncing for the next account', async () => {
    getPreferences.mockRejectedValue(new Error('404 not found'));
    await loadServerPrefs();
    resetServerPrefs();

    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledTimes(1);
  });
});
