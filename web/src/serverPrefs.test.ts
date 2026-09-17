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
//   1. a burst of adjustments coalesces into ONE PUT — a drag emits a value per frame, and each PUT
//      is a round trip and a database write (it wrote an audit row too, until ADR-154);
//   2. a failed GET stops later saves for the session — otherwise a deployment on an N-1 core PUTs
//      into a 404 every 800ms for as long as someone is dragging;
//   3. sign-out cancels a pending save — a save queued by the previous account must not land on the
//      next one's row;
//   4. an empty collapsed-folder layout is sent only by a browser that has held one (ADR-154) —
//      sending `{}` from a machine that never touched the tree reopens every folder closed elsewhere.

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
import {
  loadServerPrefs,
  resetServerPrefs,
  mergeTableColumnWidths,
  setInterfaceDockHeight,
  setNodeTreeCollapsed,
  setNodeTreePinnedOnly,
  setNodeTreeWithNodesOnly,
} from './serverPrefs';
import { MAX_STORED_COLLAPSED } from './lib/nodeTree';
import { COLUMN_MAX_PX, MAX_STORED_COLUMNS, MAX_STORED_TABLES } from './lib/columnWidths';
import type { TableId } from './lib/tableIds';

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
  usePrefsStore.getState().setNodeTreePinnedOnly(null);
  usePrefsStore.getState().setNodeTreeWithNodesOnly(null);
  usePrefsStore.getState().setNodeTreeCollapsed({});
  usePrefsStore.getState().setTableColumnWidths({});
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

describe('the Pinned only switch (ADR-146)', () => {
  it('adopts the account\'s answer, and nothing that is not a boolean', async () => {
    getPreferences.mockResolvedValue({ nodeTreePinnedOnly: true });
    await loadServerPrefs();
    expect(usePrefsStore.getState().nodeTreePinnedOnly).toBe(true);
    for (const value of ['true', 1, null, {}]) {
      getPreferences.mockResolvedValue({ nodeTreePinnedOnly: value });
      await loadServerPrefs();
      expect(usePrefsStore.getState().nodeTreePinnedOnly).toBe(true);
    }
  });

  it('saves switching it off, not only on', () => {
    // Omitting `false` would leave the account saying "on" to the next machine.
    setNodeTreePinnedOnly(false);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({ nodeTreePinnedOnly: false });
  });

  it('sends nothing for a machine that never touched it', () => {
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({ interfaceDockHeight: 360 });
  });
});

describe('the Folders-with-nodes-only switch (ADR-159)', () => {
  it('adopts the account\'s answer, and nothing that is not a boolean', async () => {
    getPreferences.mockResolvedValue({ nodeTreeWithNodesOnly: true });
    await loadServerPrefs();
    expect(usePrefsStore.getState().nodeTreeWithNodesOnly).toBe(true);
    for (const value of ['true', 1, null, {}]) {
      getPreferences.mockResolvedValue({ nodeTreeWithNodesOnly: value });
      await loadServerPrefs();
      expect(usePrefsStore.getState().nodeTreeWithNodesOnly).toBe(true);
    }
  });

  it('saves switching it off, not only on', () => {
    setNodeTreeWithNodesOnly(false);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({ nodeTreeWithNodesOnly: false });
  });

  it('travels beside the other switch rather than replacing it', () => {
    // Both are the inventory tree's, and one document carries the pair — a save that dropped the
    // other would switch it off on the next machine.
    setNodeTreePinnedOnly(true);
    setNodeTreeWithNodesOnly(true);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({
      nodeTreePinnedOnly: true,
      nodeTreeWithNodesOnly: true,
    });
  });

  it('sends nothing for a machine that never touched it', () => {
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({ interfaceDockHeight: 360 });
  });
});

describe('the inventory tree\'s collapsed folders (ADR-154)', () => {
  const layout = () => usePrefsStore.getState().nodeTreeCollapsed;

  it('adopts the account\'s layout over this browser\'s', async () => {
    usePrefsStore.getState().setNodeTreeCollapsed({ local: true });
    getPreferences.mockResolvedValue({ nodeTreeCollapsed: { g1: true, g2: false } });
    await loadServerPrefs();
    expect(layout()).toEqual({ g1: true });
  });

  it('keeps this browser\'s layout when the account holds none, or nothing that reads as one', async () => {
    for (const body of [{}, { nodeTreeCollapsed: 'g1' }, { nodeTreeCollapsed: ['g1'] }]) {
      usePrefsStore.getState().setNodeTreeCollapsed({ local: true });
      getPreferences.mockResolvedValue(body);
      await loadServerPrefs();
      expect(layout()).toEqual({ local: true });
    }
  });

  it('saves a press, and sends nothing for a press that changes nothing', () => {
    // `pressTwisty` hands back the stored object for a no-op press; that must cost no request.
    setNodeTreeCollapsed(layout());
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).not.toHaveBeenCalled();

    setNodeTreeCollapsed({ g1: true });
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledTimes(1);
    expect(putPreferences).toHaveBeenCalledWith({ nodeTreeCollapsed: { g1: true } });
  });

  it('sends the empty layout once the last folder is opened again', () => {
    // Omitting `{}` would leave the account saying "closed" to the next machine.
    setNodeTreeCollapsed({ g1: true });
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    setNodeTreeCollapsed({});
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenLastCalledWith({ nodeTreeCollapsed: {} });
  });

  it('carries the account\'s empty layout along when something else is saved', async () => {
    getPreferences.mockResolvedValue({ nodeTreeCollapsed: {} });
    await loadServerPrefs();
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({ interfaceDockHeight: 360, nodeTreeCollapsed: {} });
  });

  it('seeds the account with a layout this browser kept from before the upgrade', async () => {
    usePrefsStore.getState().setNodeTreeCollapsed({ g1: true });
    getPreferences.mockResolvedValue({});
    await loadServerPrefs();
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({
      interfaceDockHeight: 360,
      nodeTreeCollapsed: { g1: true },
    });
  });

  it('a browser that never held a layout does not send one, even after a sign-out', async () => {
    getPreferences.mockResolvedValue({ nodeTreeCollapsed: {} });
    await loadServerPrefs();
    resetServerPrefs();
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences).toHaveBeenCalledWith({ interfaceDockHeight: 360 });
  });
});

describe('a save made while the account is still being read (ADR-154 decision 9)', () => {
  /** A GET that answers only when the test says so. */
  function heldLoad() {
    let answer: (body: unknown) => void = () => undefined;
    let fail: (e: Error) => void = () => undefined;
    getPreferences.mockReturnValue(
      new Promise((resolve, reject) => {
        answer = resolve;
        fail = reject;
      }),
    );
    return { load: loadServerPrefs(), answer: (b: unknown) => answer(b), fail: (e: Error) => fail(e) };
  }

  it('waits for the load, then sends what the load adopted — once', async () => {
    const held = heldLoad();
    setNodeTreeCollapsed({ pressed: true });
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    expect(putPreferences, 'a save went out before the account was read').not.toHaveBeenCalled();

    held.answer({ nodeTreeCollapsed: { g2: true } });
    await held.load;
    await vi.runAllTimersAsync();
    expect(putPreferences).toHaveBeenCalledTimes(1);
    expect(putPreferences).toHaveBeenCalledWith({ nodeTreeCollapsed: { g2: true } });
  });

  it('sends nothing when the load finds no endpoint', async () => {
    const held = heldLoad();
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    held.fail(new Error('404 not found'));
    await held.load;
    await vi.runAllTimersAsync();
    expect(putPreferences).not.toHaveBeenCalled();
  });

  it('sends nothing when the operator signs out while it waits', async () => {
    const held = heldLoad();
    setInterfaceDockHeight(360);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);
    resetServerPrefs();
    held.answer({});
    await held.load;
    await vi.runAllTimersAsync();
    expect(putPreferences).not.toHaveBeenCalled();
  });
});

describe('the whole document, saturated', () => {
  it('stays inside the endpoint\'s 32 KiB with every capped preference full', () => {
    // 🚨 The assertion the two caps are sized by (ADR-154 decision 6). `PUT /api/v1/preferences`
    // refuses a body over `MAX_USER_PREFS_BYTES` (crates/yagra-core/src/api/preferences.rs) — this
    // test restates that number rather than reading it, so if the Rust constant moves, this is the
    // other place that has to. Every table id and column key is padded past anything real, and each
    // folder id is a full 36-character UUID.
    const widths: Record<string, Record<string, number>> = {};
    for (let t = 0; t < MAX_STORED_TABLES; t += 1) {
      const table: Record<string, number> = {};
      for (let c = 0; c < MAX_STORED_COLUMNS; c += 1) {
        table[`column_key_padded_${String(c).padStart(4, '0')}`] = COLUMN_MAX_PX;
      }
      widths[`table_id_padded_to_thirty_two_${String(t).padStart(4, '0')}`] = table;
    }
    usePrefsStore.getState().setTableColumnWidths(widths);
    const collapsed: Record<string, true> = {};
    for (let i = 0; i < MAX_STORED_COLLAPSED; i += 1) {
      collapsed[`00000000-0000-4000-8000-${String(i).padStart(12, '0')}`] = true;
    }
    usePrefsStore.getState().setNodeTreePinnedOnly(true);
    setNodeTreeCollapsed(collapsed);
    // One more width through the real setter, so the document is the one the product would send.
    mergeTableColumnWidths('events.log' as TableId, { time: COLUMN_MAX_PX });
    setInterfaceDockHeight(99_999);
    vi.advanceTimersByTime(SAVE_DEBOUNCE_MS);

    expect(putPreferences).toHaveBeenCalledTimes(1);
    const body = putPreferences.mock.calls[0][0] as Record<string, unknown>;
    expect(Object.keys(body.nodeTreeCollapsed as object)).toHaveLength(MAX_STORED_COLLAPSED);
    const bytes = new TextEncoder().encode(JSON.stringify(body)).length;
    expect(bytes).toBeLessThan(32 * 1024);
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
